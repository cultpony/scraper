use std::time::Instant;

use anyhow::Result;
use envconfig::Envconfig;
use flexi_logger::LoggerHandle;
use lazy_static::lazy_static;
use log::{info, trace, LevelFilter};
use std::sync::Mutex;
use tide::Request;

use crate::scraper::ScrapeResult;

mod camo;
mod scraper;

#[derive(serde::Deserialize, Clone)]
pub struct ScrapeRequest {
    url: String,
    #[serde(alias = "_method")]
    _method: Option<String>,
}

async fn verify_origin(req: &Request<State>) -> bool {
    let origin = req
        .header("Origin")
        .map(|x| x.as_str())
        .unwrap_or(":no-host-origin");
    req.state().is_allowed_origin(origin)
}

async fn scrape_post(mut req: Request<State>) -> tide::Result {
    if !verify_origin(&req).await {
        return Err(tide::Error::from(anyhow::Error::msg("access denied")));
    }
    let scrape_req: ScrapeRequest = req.body_json().await?;
    scrape_inner(
        &req.state().config,
        req.state().result_cache.clone(),
        scrape_req,
    )
    .await
}

async fn scrape(req: Request<State>) -> tide::Result {
    if !verify_origin(&req).await {
        return Err(tide::Error::from(anyhow::Error::msg("access denied")));
    }
    let scrape_req: ScrapeRequest = req.query()?;
    scrape_inner(
        &req.state().config,
        req.state().result_cache.clone(),
        scrape_req,
    )
    .await
}

async fn scrape_inner(
    config: &Configuration,
    request_cache: ResultCache,
    scrape_req: ScrapeRequest,
) -> tide::Result {
    let url = scrape_req.url.clone();
    let res: std::result::Result<Option<ScrapeResult>, std::sync::Arc<anyhow::Error>> =
        request_cache
            .try_get_with(scrape_req.url, scraper::scrape(config, &url))
            .await;
    let res = match res {
        Ok(r) => r,
        Err(e) => {
            let e = ScrapeResult::from_err(e);
            return Ok(tide::Response::builder(200)
                .body(serde_json::to_string(&e)?)
                .content_type(tide::http::mime::JSON)
                .build());
        }
    };
    let res = match res {
        Some(res) => res,
        None => {
            return Ok(tide::Response::builder(200)
                .body(serde_json::to_string(&ScrapeResult::Err(
                    "URL invalid".to_string().into(),
                ))?)
                .content_type(tide::http::mime::JSON)
                .build())
        }
    };
    Ok(tide::Response::builder(200)
        .body(serde_json::to_string(&res)?)
        .content_type(tide::http::mime::JSON)
        .build())
}

#[derive(Envconfig, Clone, securefmt::Debug)]
pub struct Configuration {
    #[envconfig(from = "LISTEN_ON", default = "localhost:8080")]
    #[sensitive]
    bind_to: std::net::SocketAddr,
    #[envconfig(from = "ALLOWED_ORIGINS", default = "localhost,localhost:8080")]
    #[sensitive]
    allowed_origins: String,
    #[envconfig(from = "CHECK_CSRF_PRESENCE", default = "false")]
    check_csrf_presence: bool,
    #[envconfig(from = "TUMBLR_API_KEY")]
    #[sensitive]
    tumblr_api_key: Option<String>,
    #[envconfig(from = "HTTP_PROXY")]
    #[sensitive]
    proxy_url: Option<String>,
    #[envconfig(from = "CAMO_KEY")]
    #[sensitive]
    camo_key: Option<String>,
    #[envconfig(from = "CAMO_HOST")]
    camo_host: Option<String>,
    #[envconfig(from = "ENABLE_GET_REQUEST", default = "false")]
    enable_get_request: bool,
    #[envconfig(from = "PREFERRED_NITTER_INSTANCE_HOST")]
    preferred_nitter_instance_host: Option<String>,
    #[envconfig(from = "LOG_LEVEL", default = "INFO")]
    log_level: LevelFilter,
}

#[derive(Clone)]
pub struct State {
    config: Configuration,
    parsed_allowed_origins: Vec<String>,
    result_cache: ResultCache,
}

pub type ResultCache = moka::future::Cache<String, Option<scraper::ScrapeResult>>;

impl State {
    fn new(config: Configuration) -> Result<Self> {
        Ok(Self {
            parsed_allowed_origins: config
                .allowed_origins
                .split(',')
                .filter(|x| !x.is_empty())
                .map(|x| x.to_string())
                .collect(),
            config,
            result_cache: moka::future::CacheBuilder::new(1000)
                .initial_capacity(1000)
                .support_invalidation_closures()
                .time_to_idle(std::time::Duration::from_secs(10 * 60))
                .time_to_live(std::time::Duration::from_secs(100 * 60))
                .build(),
        })
    }
    pub fn is_allowed_origin(&self, origin: &str) -> bool {
        let mut allowed = false;
        for host in &self.parsed_allowed_origins {
            if host == origin {
                allowed = true;
            }
        }
        allowed || self.parsed_allowed_origins.is_empty()
    }
}

impl Default for Configuration {
    fn default() -> Self {
        let s = Self {
            bind_to: std::net::ToSocketAddrs::to_socket_addrs("localhost:8080")
                .unwrap()
                .next()
                .unwrap(),
            allowed_origins: "".to_string(),
            check_csrf_presence: false,
            tumblr_api_key: std::env::var("TUMBLR_API_KEY").ok(),
            proxy_url: None,
            camo_host: None,
            camo_key: None,
            enable_get_request: false,
            preferred_nitter_instance_host: None,
            log_level: LevelFilter::Info,
        };
        trace!("created config: {:?}", s);
        s
    }
}

fn main() -> Result<()> {
    crate::LOGGER.lock().unwrap().flush();
    use tokio::runtime::Builder;
    let runtime = Builder::new_multi_thread()
        .worker_threads(16)
        .max_blocking_threads(64)
        .on_thread_stop(|| {
            log::trace!("thread stopping");
        })
        .on_thread_start(|| {
            log::trace!("thread started");
        })
        .thread_name_fn(|| {
            use std::sync::atomic::{AtomicUsize, Ordering};
            static ATOMIC_ID: AtomicUsize = AtomicUsize::new(0);
            let id = ATOMIC_ID.fetch_add(1, Ordering::SeqCst);
            format!("philomena-scraper-{}", id)
        })
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async move { tokio::spawn(async move { main_start().await }).await? })?;
    runtime.shutdown_timeout(std::time::Duration::from_secs(10));
    Ok(())
}

struct RequestTimer();

#[tide::utils::async_trait]
impl<State: Clone + Send + Sync + 'static> tide::Middleware<State> for RequestTimer {
    async fn handle(&self, req: Request<State>, next: tide::Next<'_, State>) -> tide::Result {
        let start = Instant::now();
        let mut res = next.run(req).await;
        let time_taken = Instant::now().duration_since(start);
        res.insert_header(
            "x-time-taken",
            format!("{:1.3}ms", time_taken.as_secs_f32() * 1000.0),
        );

        Ok(res)
    }
}

async fn main_start() -> Result<()> {
    let config = Configuration::init_from_env();
    let config = match config {
        Err(e) => {
            log::error!("could not load config: {}", e);
            Configuration::default()
        }
        Ok(v) => v,
    };
    log::info!("log level is now {}", config.log_level);
    LOGGER.lock().unwrap().set_new_spec(
        flexi_logger::LogSpecification::builder()
            .default(LevelFilter::Warn)
            .module("scraper", config.log_level)
            .build(),
    );
    let mut app = tide::with_state(State::new(config.clone())?);
    app.with(RequestTimer());
    app.at("/images/scrape").post(scrape_post);
    if config.enable_get_request {
        app.at("/images/scrape").get(scrape);
    }
    app.listen(config.bind_to).await?;
    Ok(())
}

lazy_static! {
    static ref LOGGER: Mutex<LoggerHandle> = {
        better_panic::install();
        if let Err(e) = kankyo::load(false) {
            info!("couldn't load .env file: {}, this is probably fine", e);
        }
        Mutex::new(
            flexi_logger::Logger::with(
                flexi_logger::LogSpecification::builder()
                    .default(LevelFilter::Warn)
                    .module("scraper", LevelFilter::Info)
                    .build(),
            )
            .start()
            .unwrap(),
        )
    };
}
