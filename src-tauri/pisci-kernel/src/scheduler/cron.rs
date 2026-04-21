/// Cron scheduler using tokio-cron-scheduler
use anyhow::Result;
use tokio_cron_scheduler::{Job, JobScheduler};
use tracing::info;

pub struct CronScheduler {
    scheduler: JobScheduler,
}

impl CronScheduler {
    pub async fn new() -> Result<Self> {
        let scheduler = JobScheduler::new().await?;
        Ok(Self { scheduler })
    }

    pub async fn start(&self) -> Result<()> {
        self.scheduler.start().await?;
        info!("Cron scheduler started");
        Ok(())
    }

    pub async fn shutdown(&mut self) -> Result<()> {
        self.scheduler.shutdown().await?;
        Ok(())
    }

    /// Add a job with a cron expression
    /// cron format: "sec min hour day month weekday"
    pub async fn add_job<F>(&self, cron_expr: &str, task_id: String, f: F) -> Result<uuid::Uuid>
    where
        F: Fn(
                uuid::Uuid,
                JobScheduler,
            ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>
            + Send
            + Sync
            + 'static,
    {
        // tokio-cron-scheduler uses 7-part cron (sec min hour day month weekday year)
        // Convert 5-part (min hour day month weekday) to 7-part
        let full_cron = if cron_expr.split_whitespace().count() == 5 {
            format!("0 {}", cron_expr)
        } else {
            cron_expr.to_string()
        };

        use std::sync::Arc;
        let f = Arc::new(tokio::sync::Mutex::new(f));
        let job = Job::new_async(full_cron.as_str(), move |uuid, sched| {
            let task_id = task_id.clone();
            let f = f.clone();
            Box::pin(async move {
                info!("Running scheduled task: {}", task_id);
                let guard = f.lock().await;
                guard(uuid, sched).await;
            })
        })?;

        let id = self.scheduler.add(job).await?;
        Ok(id)
    }

    pub async fn remove_job(&self, job_id: uuid::Uuid) -> Result<()> {
        self.scheduler.remove(&job_id).await?;
        Ok(())
    }
}
