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
    static ref MEDIA_REGEX: Regex = Regex::from_str(r#"(https?://(?:\d+\.)?media\.tumblr\.com/[a-f\d]+/[a-f\d]+-[a-f\d]+/s\d+x\d+/[a-f\d]+\.(?:png|jpe?g|gif))"#)
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
    Ok(false)
}

async fn make_tumblr_api_request(client: &Client, api_url: &str) -> Result<Value> {
    debug!("running api request, not in cache");
    client
        .get(api_url)
        .send()
        .await
        .context("request to tumblr failed")?
        .error_for_status()
        .context("request to tumblr returned error code")?
        .json()
        .await
        .context("could not parse tumblr response as json")
}

pub async fn tumblr_scrape(config: &Configuration, url: &Url) -> Result<Option<ScrapeResult>> {
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

    let client = crate::scraper::client(config)?;
    let resp: Value = make_tumblr_api_request(&client, &api_url).await?;

    if resp["meta"]["status"] != 200 {
        anyhow::bail!("tumblr returned non-200 error");
    }

    let resp = &resp["response"]["posts"][0];

    match resp["type"].as_str() {
        Some("photo") => {
            debug!("photo post, sending to photo scraper");
            add_meta(
                resp.clone(),
                process_post(PostType::Photo, resp.clone(), config, &client).await?,
            )
            .await
        }
        Some("text") => {
            debug!("text post, sending to post scraper");
            add_meta(
                resp.clone(),
                process_post(PostType::Text, resp.clone(), config, &client).await?,
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
    config: &Configuration,
    client: &Client,
) -> Result<Option<Vec<ScrapeImage>>> {
    match post_type {
        PostType::Photo => process_post_photo(post, config, client).await,
        PostType::Text => process_post_text(post, config).await,
    }
}

async fn process_post_text(
    post: Value,
    config: &Configuration,
) -> Result<Option<Vec<ScrapeImage>>> {
    let body = post
        .get("body")
        .and_then(|x| x.as_str())
        .unwrap_or_default();
    println!("{:?}", body);
    let media_regex = MEDIA_REGEX.clone();
    let images = media_regex.captures(body);
    let images = match images {
        None => return Ok(None),
        Some(v) => v,
    };
    let mut images: Vec<&str> = images
        .iter()
        .map(|x| x.map(|x| x.as_str()).unwrap_or_default())
        .collect();
    images.sort_unstable();
    images.dedup();
    println!("Found {} potential images", images.len());
    let mut meta_images = Vec::new();
    for i in images {
        let i = Url::from_str(i)?;
        println!("cap: {:?}", i);
        meta_images.push(ScrapeImage {
            camo_url: from_url(camo_url(config, &i)?),
            url: from_url(i),
        });
    }
    Ok(Some(meta_images))
}

async fn process_post_photo(
    post: Value,
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
                let image = upsize(photo["original_size"]["url"].clone(), config, client).await?;
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
                    .flat_map(|(image, preview)| -> Result<ScrapeImage> {
                        Ok(ScrapeImage {
                            url: from_url(image.clone()),
                            camo_url: from_url(camo_url(config, preview)?),
                        })
                    })
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
            let source_url = source_url.map(from_url);
            let author_name = post["blog_name"].as_str().map(|x| x.to_string());
            let description = post["summary"].as_str().map(|x| x.to_string());

            Ok(Some(ScrapeResult::Ok(ScrapeResultData {
                source_url,
                author_name,
                additional_tags: None,
                description,
                images,
            })))
        }
    }
}

async fn upsize(image_url: Value, _config: &Configuration, client: &Client) -> Result<Option<Url>> {
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
    let tumblr_sizes = TUMBLR_SIZES.clone();
    for size in tumblr_sizes.iter() {
        let image_url = SIZE_REGEX.replace(image_url, |caps: &Captures| {
            format!("_{}{}", size, &caps[2])
        });
        let image_url = Url::from_str(&image_url)?;
        trace!("found url: {}", image_url);
        if url_ok(client, &image_url).await? {
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

#[cfg(test)]
mod test {
    use log::warn;

    use crate::scraper::scrape;

    use super::*;

    #[test]
    #[ignore]
    fn test_tumblr_scraper() -> Result<()> {
        crate::LOGGER.lock().unwrap().flush();
        let url = r#"https://tcn1205.tumblr.com/post/186904081532/in-wonderland"#;
        let config = Configuration::default();
        let api_key = config.tumblr_api_key.clone().unwrap_or_default();
        if config.tumblr_api_key.is_none() && api_key.trim().is_empty() {
            warn!("Tumblr API key not configured, skipping");
            return Ok(());
        }
        let scrape = tokio_test::block_on(scrape(&config, url));
        let scrape = match scrape {
            Ok(s) => s,
            Err(e) => return Err(e),
        };
        let scrape = match scrape {
            Some(s) => s,
            None => anyhow::bail!("got none response from scraper"),
        };
        let expected_result = ScrapeResult::Ok(ScrapeResultData{
            source_url: Some("https://tcn1205.tumblr.com/post/186904081532/in-wonderland".to_string()),
            author_name: Some("tcn1205".to_string()),
            additional_tags: None,
            description: Some("In Wonderland.".to_string()),
            images: vec![
                ScrapeImage{
                    url: "https://64.media.tumblr.com/cf3b6e5981e0aaf0f1be305429faa6c4/tumblr_pw0dzrDNvN1vlyxx7o1_1280.png".to_string(),
                    camo_url: "https://64.media.tumblr.com/cf3b6e5981e0aaf0f1be305429faa6c4/tumblr_pw0dzrDNvN1vlyxx7o1_400.png".to_string(),
                }
            ],
        });
        visit_diff::assert_eq_diff!(expected_result, scrape);
        Ok(())
    }

    #[test]
    #[ignore]
    fn test_text_post_tumblr() -> Result<()> {
        crate::LOGGER.lock().unwrap().flush();
        let url = r#"https://witchtaunter.tumblr.com/post/182898769998/yes-this-is-horse"#;
        let config = Configuration::default();
        let api_key = config.tumblr_api_key.clone().unwrap_or_default();
        if config.tumblr_api_key.is_none() && api_key.trim().is_empty() {
            warn!("Tumblr API key not configured, skipping");
            return Ok(());
        }
        let scrape = tokio_test::block_on(scrape(&config, url));
        let scrape = match scrape {
            Ok(s) => s,
            Err(e) => return Err(e),
        };
        let scrape = match scrape {
            Some(s) => s,
            None => anyhow::bail!("got none response from scraper"),
        };
        let expected_result = ScrapeResult::Ok(ScrapeResultData{
            source_url: Some("https://witchtaunter.tumblr.com/post/182898769998/yes-this-is-horse".to_string()),
            author_name: Some("witchtaunter".to_string()),
            additional_tags: None,
            description: Some("Yes, this is horse".to_string()),
            images: vec![
                ScrapeImage{
                    url: "https://64.media.tumblr.com/fbe494244d7e68e98e59141db4fddab7/tumblr_pn53n8VjWJ1s8a9ojo1_1280.png".to_string(),
                    camo_url: "https://64.media.tumblr.com/fbe494244d7e68e98e59141db4fddab7/tumblr_pn53n8VjWJ1s8a9ojo1_400.png".to_string(),
                }
            ],
        });
        visit_diff::assert_eq_diff!(expected_result, scrape);
        Ok(())
    }
}
