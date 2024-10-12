use anyhow::{Result, anyhow};
use chrono::Local;
use cli::{Command, Opt, StartOpt};
use compose_rs::{Compose, ComposeCommand};
use env_logger::Builder;
use gcs::upload_to_gcs;
use log::{error, info, LevelFilter};
use object_store::gcp::GoogleCloudStorageBuilder;
use std::io::Write;
use structopt::StructOpt;
use tar::create_tar_lz4;
use tokio_cron_scheduler::{Job, JobScheduler};

mod cli;
mod gcs;
mod tar;

async fn create_snapshot(node_path: &str, gcs_enabled: bool, gcs_bucket: Option<String>, gcs_folder: Option<String>) -> Result<()> {
    let compose_path = format!("{}/docker-compose.yml", node_path);
    let compose = Compose::builder().path(compose_path).build()?;

    let date = Local::now().format("%d-%m-%y_%H-%M").to_string();
    let beacond_file_name = format!("{}_{}.tar.lz4", "pruned_snapshot", date);
    let reth_file_name = format!("{}_{}.tar.lz4", "reth_snapshot", date);

    info!("Stopping services in {}", node_path);
    compose.down().exec()?;

    info!("Archiving a beacond snapshot");
    create_tar_lz4(
        node_path,
        &beacond_file_name,
        &["./data/beacond/data"],
        &["priv_validator_state.json"],
    )?;
    info!("Archiving a reth snapshot");
    create_tar_lz4(
        node_path,
        &reth_file_name,
        &["./data/reth/static_files", "./data/reth/db"],
        &[],
    )?;

    info!("Starting services in {}", node_path);
    compose.up().exec()?;

    if !gcs_enabled {
        return Ok(());
    }

    let gcs_bucket = gcs_bucket.ok_or_else(|| anyhow!("GCS_BUCKET is not set"))?;
    let gcs_folder = gcs_folder.ok_or_else(|| anyhow!("GCS_FOLDER is not set"))?;

    let gcs = GoogleCloudStorageBuilder::from_env()
        .with_bucket_name(&gcs_bucket)
        .build()?;

    upload_to_gcs(&gcs, &gcs_bucket, &gcs_folder, &beacond_file_name).await?;
    upload_to_gcs(&gcs, &gcs_bucket, &gcs_folder, &reth_file_name).await?;

    Ok(())
}

fn setup_logger() -> Result<()> {
    let mut builder = Builder::from_default_env();
    builder.format(|buf, record| {
        writeln!(
            buf,
            "{} [{}] - {}",
            Local::now().format("%Y-%m-%d %H:%M:%S"),
            record.level(),
            record.args()
        )
    });
    builder.filter(None, LevelFilter::Info);
    builder.init();
    Ok(())
}

pub async fn start_scheduler(opt: StartOpt) -> Result<()> {
    let sched = JobScheduler::new().await?;

    let job = Job::new_async(opt.job_time.as_str(), move |_uuid, _l| {
        let path = opt.path.clone();
        let gcs_enabled = opt.gcs;
        let bucket = opt.gcs_bucket.clone();
        let gcs_folder = opt.gcs_folder.clone();

        Box::pin(async move {
            if let Err(e) = create_snapshot(&path, gcs_enabled, bucket, gcs_folder).await {
                error!("Error during snapshot creation and upload: {}", e);
            }
        })
    })?;

    sched.add(job).await?;
    sched.start().await?;

    tokio::signal::ctrl_c().await?;
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    setup_logger()?;
    let opt = Opt::from_args();

    match opt.cmd {
        Command::Start(start_opt) => {
            start_scheduler(start_opt).await?;
        }
    }

    Ok(())
}
