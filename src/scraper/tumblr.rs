use futures_cache::{Cache, Duration};
use log::{debug, trace};
use reqwest::Client;
use std::str::FromStr;

use crate::{
    camo::camo_url,
    scraper::{from_url, ScrapeImage, ScrapeResult, ScrapeResultData},
    Configuration,
};
use anyhow::{Context, Result};
use ipnet::IpNet;
use regex::{Captures, Regex};
use serde_json::Value;
use url::Url;

lazy_static::lazy_static! {
    static ref URL_REGEX: Regex = Regex::from_str(r#"https?://(.*)/(image|post)/(\d+).*"#)
        .expect("failure in setting up essential regex");
    static ref MEDIA_REGEX: Regex = Regex::from_str(r#"https?://(?:\d+\.)?media\.tumblr\.com/[a-f\d]+/[a-f\d]+-[a-f\d]+/s\d+x\d+/[a-f\d]+\.(?:png\|jpe?g\|gif)"#)
        .expect("failure in setting up essential regex");
    static ref SIZE_REGEX: Regex = Regex::from_str(r#"_(\d+)(\..+)\z"#)
        .expect("failure in setting up essential regex");
    static ref TUMBLR_RANGES: Vec<IpNet> = IpNet::aggregate(&Vec::from([
        IpNet::from_str("66.6.32.0/24").unwrap(),
        IpNet::from_str("66.6.33.0/24").unwrap(),
        IpNet::from_str("66.6.44.0/24").unwrap(),
        IpNet::from_str("74.114.152.0/24").unwrap(),
        IpNet::from_str("74.114.153.0/24").unwrap(),
        IpNet::from_str("74.114.154.0/24").unwrap(),
        IpNet::from_str("74.114.155.0/24").unwrap(),
    ]));
    static ref TUMBLR_SIZES: Vec<u64> = vec![1280, 540, 500, 400, 250, 100, 75];
}

pub async fn is_tumblr(url: &Url) -> Result<bool> {
    if URL_REGEX.is_match_at(url.as_str(), 0) {
        trace!("tumblr matched on regex URL");
        return Ok(true);
    }
    trace!("tumblr didn't match on regex, trying host resolver");
    Ok(match url.host() {
        Some(host) => tumblr_domain(host).await?,
        None => false,
    })
}

async fn tumblr_domain(host: url::Host<&str>) -> Result<bool> {
    let hosts = dns_lookup::lookup_host(&host.to_string())?;
    trace!("got hosts for URL: {:?}", hosts);
    for host in hosts {
        if TUMBLR_RANGES.iter().any(|net| net.contains(&host)) {
            return Ok(true);
        }
    }
    trace!("host not in URL list");
    return Ok(false);
}

async fn make_tumblr_api_request(client: &Client, api_url: &str) -> Result<Value> {
    debug!("running api request, not in cache");
    Ok(client
        .get(api_url)
        .send()
        .await
        .context("request to tumblr failed")?
        .error_for_status()
        .context("request to tumblr returned error code")?
        .json()
        .await
        .context("could not parse tumblr response as json")?)
}

pub async fn tumblr_scrape(
    config: &Configuration,
    url: &Url,
    db: &sled::Db,
) -> Result<Option<ScrapeResult>> {
    let reqwest_cache = Cache::load(db.open_tree("tumblr_request_cache")?)?;

    trace!("analyzing tumblr url {}", url);
    let post_id = URL_REGEX.captures(url.as_str());
    let post_id = match post_id {
        None => return Ok(None),
        Some(p) => p,
    };
    trace!("captured: {:?}", post_id);
    let post_id = &post_id[3];
    trace!("tumblr blog id: {}", post_id);
    let api_key = config.tumblr_api_key.as_ref();
    let api_key = match api_key {
        None => "",
        Some(s) => s.as_str(),
    };
    let host = url.host_str();
    let host = match host {
        None => return Ok(None),
        Some(p) => p,
    };
    let api_url = format!(
        r#"https://api.tumblr.com/v2/blog/{host}/posts/photo?id={post_id}&api_key={api_key}"#,
        host = host,
        post_id = post_id,
        api_key = api_key
    );
    debug!("Requesting API URL {} from Tumblr", api_url);

    let client = crate::scraper::client(config)?;
    let resp: Value = reqwest_cache
        .wrap(
            (&api_url, "head"),
            Duration::seconds(config.cache_http_duration as i64),
            make_tumblr_api_request(&client, &api_url),
        )
        .await?;

    if resp["meta"]["status"] != 200 {
        anyhow::bail!("tumblr returned non-200 error");
    }

    let resp = &resp["response"]["posts"][0];

    match resp["type"].as_str() {
        Some("photo") => {
            debug!("photo post, sending to photo scraper");
            add_meta(
                resp.clone(),
                process_post(PostType::Photo, resp.clone(), db, config, &client).await?,
            )
            .await
        }
        Some("text") => {
            debug!("text post, sending to post scraper");
            add_meta(
                resp.clone(),
                process_post(PostType::Text, resp.clone(), db, config, &client).await?,
            )
            .await
        }
        _ => {
            debug!("Post is type {}, couldn't handle that", resp["type"]);
            Ok(None)
        }
    }
}

enum PostType {
    Photo,
    Text,
}

async fn process_post(
    post_type: PostType,
    post: Value,
    db: &sled::Db,
    config: &Configuration,
    client: &Client,
) -> Result<Option<Vec<ScrapeImage>>> {
    match post_type {
        PostType::Photo => process_post_photo(post, db, config, client).await,
        PostType::Text => {
            //TODO: implement tumblr text scraping
            unimplemented!()
        }
    }
}

async fn process_post_photo(
    post: Value,
    db: &sled::Db,
    config: &Configuration,
    client: &Client,
) -> Result<Option<Vec<ScrapeImage>>> {
    let photos = post["photos"].as_array();
    match photos {
        None => {
            debug!("found no photos, bailing");
            Ok(None)
        }
        Some(photos) => {
            let mut images = Vec::new();
            for photo in photos.iter() {
                debug!("upsizing photo {}", photo);
                let image =
                    upsize(photo["original_size"]["url"].clone(), db, config, client).await?;
                let image = match image {
                    None => continue,
                    Some(i) => i,
                };
                let alt_sizes = photo["alt_sizes"].as_array();
                let preview = match alt_sizes {
                    None => None,
                    Some(alt_sizes) => {
                        let mut valid_alt_sizes = Vec::new();
                        for alt_size in alt_sizes {
                            if alt_size["width"] == 400 {
                                let url = alt_size["url"].as_str();
                                match url {
                                    None => (),
                                    Some(url) => {
                                        valid_alt_sizes.push(Url::from_str(url)?);
                                    }
                                }
                            }
                        }
                        if valid_alt_sizes.is_empty() {
                            valid_alt_sizes.push(image.clone());
                        }
                        valid_alt_sizes.pop()
                    }
                };
                match preview {
                    None => images.push((image.clone(), image)),
                    Some(preview) => images.push((image, preview)),
                }
            }
            Ok(Some(
                images
                    .iter()
                    .map(|(image, preview)| -> Result<ScrapeImage> {
                        Ok(ScrapeImage {
                            url: from_url(image.clone()),
                            camo_url: from_url(camo_url(config, preview)?),
                        })
                    })
                    .flatten()
                    .collect(),
            ))
        }
    }
}

async fn add_meta(post: Value, images: Option<Vec<ScrapeImage>>) -> Result<Option<ScrapeResult>> {
    match images {
        None => Ok(None),
        Some(images) => {
            let source_url = post["post_url"].as_str().map(|x| x.to_string());
            let source_url = source_url.map(|x| Url::from_str(&x)).transpose()?;
            let source_url = source_url.map(|x| from_url(x));
            let author_name = post["blog_name"].as_str().map(|x| x.to_string());
            let description = post["summary"].as_str().map(|x| x.to_string());

            Ok(Some(ScrapeResult::Ok(ScrapeResultData {
                author_name,
                source_url,
                description,
                images,
            })))
        }
    }
}

async fn upsize(
    image_url: Value,
    db: &sled::Db,
    config: &Configuration,
    client: &Client,
) -> Result<Option<Url>> {
    let url_ok_cache = Cache::load(db.open_tree("tumblr_url_ok_cache")?)?;

    let image_url = image_url.as_str();
    let image_url = match image_url {
        None => {
            trace!("no upsized image, returning");
            return Ok(None);
        }
        Some(i) => i,
    };
    debug!("mapping {:?} to alt_size", image_url);
    let mut urls = Vec::new();
    for size in TUMBLR_SIZES.iter() {
        let image_url = SIZE_REGEX.replace(image_url, |caps: &Captures| {
            format!("_{}{}", size, &caps[2])
        });
        let image_url = Url::from_str(&image_url)?;
        trace!("found url: {}", image_url);
        if url_ok_cache
            .wrap(
                &image_url,
                Duration::seconds(config.cache_http_duration as i64),
                url_ok(&client, &image_url),
            )
            .await?
        {
            trace!("url found valid: {}", image_url);
            urls.push(image_url);
        }
    }
    match urls.first() {
        None => Ok(None),
        Some(image_url) => Ok(Some(image_url.clone())),
    }
}

async fn url_ok(client: &Client, url: &Url) -> Result<bool> {
    let resp = client.head(url.clone()).send().await?;
    trace!("checking url {} for response", url);
    if resp.status() == 200 {
        trace!("url {} was ok", url);
        Ok(true)
    } else {
        trace!("url {} was not ok: {}", url, resp.status());
        Ok(false)
    }
}
