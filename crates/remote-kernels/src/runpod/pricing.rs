//! What a pod's own disks cost. `RunPod`'s API prices GPUs but not storage, so
//! this reads the table on its public pricing page, and falls back to the
//! prices it listed when this was written if that page can't be read.

use std::time::Duration;

const PRICING_PAGE: &str = "https://docs.runpod.io/pods/pricing.md";
const FETCH_TIMEOUT: Duration = Duration::from_secs(5);

/// `RunPod` bills disks per second but lists them per month, without saying how
/// long a month is. This is a calendar year's average.
const HOURS_PER_MONTH: f64 = 730.0;

/// A price outside this range means the page changed shape, not that disks
/// became free or cost more than a GPU.
const PLAUSIBLE_PER_GB_MONTH: std::ops::RangeInclusive<f64> = 0.001..=5.0;

/// Listed prices, in dollars per GB per month.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StoragePrices {
    pub container_running: f64,
    pub volume_running: f64,
    pub volume_stopped: f64,
}

impl StoragePrices {
    /// As listed in November 2026.
    pub const LISTED: Self = Self {
        container_running: 0.10,
        volume_running: 0.10,
        volume_stopped: 0.20,
    };

    /// What these disks add per hour to a running pod.
    pub fn running_per_hr(&self, container_gb: u32, volume_gb: u32) -> f64 {
        (f64::from(container_gb) * self.container_running
            + f64::from(volume_gb) * self.volume_running)
            / HOURS_PER_MONTH
    }

    /// What a stopped pod bills per hour: only its volume outlives the stop.
    pub fn stopped_per_hr(&self, volume_gb: u32) -> f64 {
        f64::from(volume_gb) * self.volume_stopped / HOURS_PER_MONTH
    }

    /// Today's prices from the public page, or the listed ones if it can't be
    /// read or understood. Never fails: pricing mustn't stop a machine.
    pub async fn fetch() -> Self {
        match Self::fetch_from_page().await {
            Ok(prices) => prices,
            Err(error) => {
                tracing::warn!("Using the storage prices listed in the code: {error:#}");
                Self::LISTED
            }
        }
    }

    /// The page's current prices. Public so the live tests can catch the page
    /// changing shape, which [`Self::fetch`] would only log.
    #[doc(hidden)]
    pub async fn fetch_from_page() -> anyhow::Result<Self> {
        let page = reqwest::Client::builder()
            .timeout(FETCH_TIMEOUT)
            .build()?
            .get(PRICING_PAGE)
            .send()
            .await?
            .error_for_status()?
            .text()
            .await?;
        Self::parse(&page)
            .ok_or_else(|| anyhow::anyhow!("{PRICING_PAGE} has no storage table we can read"))
    }

    /// Reads rows like
    /// `| **Volume disk** | \$0.10/GB/month | \$0.20/GB/month | Persistent |`,
    /// whose columns are the disk, its running price and its stopped price.
    fn parse(page: &str) -> Option<Self> {
        let prices_of = |disk: &str| {
            let row = page.lines().find(|line| {
                line.trim_start().starts_with('|') && line.to_lowercase().contains(disk)
            })?;
            let mut cells = row.split('|').skip(2);
            Some((cells.next()?, cells.next()?))
        };
        let (container_running, _) = prices_of("container disk")?;
        let (volume_running, volume_stopped) = prices_of("volume disk")?;
        Some(Self {
            container_running: per_gb_month(container_running)?,
            volume_running: per_gb_month(volume_running)?,
            volume_stopped: per_gb_month(volume_stopped)?,
        })
    }
}

fn per_gb_month(cell: &str) -> Option<f64> {
    let (_, price) = cell.split_once('$')?;
    let (price, _) = price.split_once("/GB")?;
    let price: f64 = price.trim().parse().ok()?;
    PLAUSIBLE_PER_GB_MONTH.contains(&price).then_some(price)
}

#[cfg(test)]
mod tests {
    use super::StoragePrices;

    const PAGE: &str = r"
## Storage pricing

| Storage type | Running Pods | Stopped Pods | Description |
|---|---|---|---|
| **Container disk** | \$0.10/GB/month | Not charged | Temporary; erased when Pod stops |
| **Volume disk** | \$0.11/GB/month | \$0.22/GB/month | Persistent; retained until Pod deleted |
| **Network volume** | \$0.07/GB/month (\< 1TB) | \$0.07/GB/month | Permanent; portable between Pods |
";

    #[test]
    fn reads_the_pricing_table() {
        assert_eq!(
            StoragePrices::parse(PAGE),
            Some(StoragePrices {
                container_running: 0.10,
                volume_running: 0.11,
                volume_stopped: 0.22,
            })
        );
    }

    #[test]
    fn a_page_it_cannot_read_gives_no_prices() {
        assert_eq!(StoragePrices::parse("no table here"), None);
        let moved = PAGE.replace("\\$0.22/GB/month", "see below");
        assert_eq!(StoragePrices::parse(&moved), None);
        let absurd = PAGE.replace("\\$0.22/GB/month", "\\$220/GB/month");
        assert_eq!(StoragePrices::parse(&absurd), None);
    }

    #[test]
    fn prices_become_hourly_rates() {
        let prices = StoragePrices::LISTED;
        assert!((prices.running_per_hr(50, 20) - 7.0 / 730.0).abs() < 1e-9);
        assert!((prices.stopped_per_hr(20) - 4.0 / 730.0).abs() < 1e-9);
    }
}
