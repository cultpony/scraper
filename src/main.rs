use std::{sync::Arc, time::Instant};

use anyhow::Result;
use axum::{
    extract::Query,
    http::Request,
    middleware::Next,
    response::IntoResponse,
    routing::get,
    Extension, Json,
};
use envconfig::Envconfig;
use flexi_logger::LoggerHandle;
use lazy_static::lazy_static;
use log::{info, trace, LevelFilter};
use std::sync::Mutex;

use crate::scraper::ScrapeResult;

mod camo;
mod scraper;

#[derive(serde::Deserialize, Clone)]
pub struct ScrapeRequest {
    url: String,
    #[serde(alias = "_method")]
    _method: Option<String>,
}

async fn origin_check<B>(req: Request<B>, state: Arc<State>, next: Next<B>) -> std::result::Result<impl axum::response::IntoResponse, axum::http::StatusCode> {
    let origin = req.headers().get("Origin").map(|x| x.to_str()).transpose();
    match origin {
        Ok(origin) => {
            if state.is_allowed_origin(origin) {
                Ok(next.run(req).await)
            } else {
                Err(axum::http::StatusCode::NOT_FOUND)
            }
        }
        Err(_) => Err(axum::http::StatusCode::INTERNAL_SERVER_ERROR),
    }
}

async fn scrape_post(
    Json(scrape_req): Json<ScrapeRequest>,
    Extension(state): Extension<Arc<State>>,
) -> axum::response::Response<String> {
    match scrape_inner(&state.config, state.result_cache.clone(), scrape_req).await {
        Ok(v) => v,
        Err(_) => todo!(),
    }
}

async fn scrape(
    Query(scrape_req): Query<ScrapeRequest>,
    Extension(state): Extension<Arc<State>>,
) -> axum::response::Response<String> {
    match scrape_inner(&state.config, state.result_cache.clone(), scrape_req).await {
        Ok(v) => v,
        Err(_) => todo!(),
    }
}

async fn scrape_inner(
    config: &Configuration,
    request_cache: ResultCache,
    scrape_req: ScrapeRequest,
) -> Result<axum::response::Response<String>> {
    let url = scrape_req.url.clone();
    let res: std::result::Result<Option<ScrapeResult>, std::sync::Arc<anyhow::Error>> =
        request_cache
            .try_get_with(scrape_req.url, scraper::scrape(config, &url))
            .await;
    let res = match res {
        Ok(r) => r,
        Err(e) => {
            let e = ScrapeResult::from_err(e);
            return Ok(axum::response::Response::builder()
                .status(axum::http::StatusCode::OK)
                .header(axum::http::header::CONTENT_TYPE, "application/json")
                .body(serde_json::to_string(&e)?)?);
        }
    };
    let res = match res {
        Some(res) => res,
        None => {
            return Ok(axum::response::Response::builder()
                .status(axum::http::StatusCode::OK)
                .header(axum::http::header::CONTENT_TYPE, "application/json")
                .body(serde_json::to_string(&ScrapeResult::Err(
                    "URL invalid".to_string().into(),
                ))?)?);
        }
    };
    Ok(axum::response::Response::builder()
        .status(axum::http::StatusCode::OK)
        .header(axum::http::header::CONTENT_TYPE, "application/json")
        .body(serde_json::to_string(&res)?)?)
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
    #[envconfig(from = "ALLOW_EMPTY_ORIGIN", default = "false")]
    allow_empty_origin: bool,
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
    pub fn is_allowed_origin(&self, origin: Option<&str>) -> bool {
        match origin {
            Some(origin) => {
                let mut allowed = false;
                for host in &self.parsed_allowed_origins {
                    if host == origin {
                        allowed = true;
                    }
                }
                allowed || self.parsed_allowed_origins.is_empty()
            },
            None => {
                self.config.allow_empty_origin
            }
        }
        
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
            allow_empty_origin: false,
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

async fn latency<B>(req: Request<B>, next: Next<B>) -> impl IntoResponse {
    let start = Instant::now();

    let mut res = next.run(req).await;

    let time_taken = start.elapsed();
    let time_taken = format!("{:1.3}ms", time_taken.as_secs_f32() * 1000.0);

    res.headers_mut().append(
        "x-time-taken",
        axum::http::HeaderValue::from_str(&*time_taken).unwrap(),
    );

    res
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
    let state = Arc::new(State::new(config.clone())?);
    let app = axum::Router::new()
        .layer(Extension(state.clone()))
        .layer(axum::middleware::from_fn(latency))
        .layer(axum::middleware::from_fn(move |a, b| {
            let state = state.clone();
            origin_check(a, state, b)
        }))
        .route("/images/scrape", get(scrape).post(scrape_post));
    axum::Server::bind(&config.bind_to)
        .serve(app.into_make_service())
        .await
        .unwrap();
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
