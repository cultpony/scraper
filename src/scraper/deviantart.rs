use crate::scraper::client;
use crate::scraper::ScrapeResultData;
use crate::{
    scraper::{from_url, ScrapeImage, ScrapeResult},
    Configuration,
};
use anyhow::Context;
use anyhow::Result;
use log::trace;
use regex::{Captures, Regex};
use std::str::FromStr;
use url::Url;

lazy_static::lazy_static! {
    static ref IMAGE_REGEX: Regex = Regex::from_str(r#"<link data-rh="true" rel="preload" href="([^"]*)" as="image"/>"#).expect("failure in setting up essential regex");
    static ref SOURCE_REGEX: Regex = Regex::from_str(r#"<link data-rh="true" rel="canonical" href="([^"]*)"/>"#).expect("failure in setting up essential regex");
    static ref ARTIST_REGEX: Regex = Regex::from_str(r#"https://www.deviantart.com/([^/]*)/art"#).expect("failure in setting up essential regex");
    static ref SERIAL_REGEX: Regex = Regex::from_str(r#"https://www.deviantart.com/(?:.*?)-(\d+)\z"#).expect("failure in setting up essential regex");
    static ref CDNINT_REGEX: Regex = Regex::from_str(r#"(https://images-wixmp-[0-9a-f]+.wixmp.com)(?:/intermediary)?/f/([^/]*)/([^/?]*)"#).expect("failure in setting up essential regex");
    static ref PNG_REGEX: Regex = Regex::from_str(r#"(https://[0-9a-z\-\.]+(?:/intermediary)?/f/[0-9a-f\-]+/[0-9a-z\-]+\.png/v1/fill/[0-9a-z_,]+/[0-9a-z_\-]+)(\.png)(.*)"#).expect("failure in setting up essential regex");
    static ref JPG_REGEX: Regex = Regex::from_str(r#"(https://[0-9a-z\-\.]+(?:/intermediary)?/f/[0-9a-f\-]+/[0-9a-z\-]+\.jpg/v1/fill/w_[0-9]+,h_[0-9]+,q_)([0-9]+)(,[a-z]+\\/[a-z0-6_\-]+\.jpe?g.*)"#).expect("failure in setting up essential regex");
}

pub async fn is_deviantart(url: &Url) -> Result<bool> {
    match url.host_str() {
        Some(url) => Ok(url.ends_with(".deviantart.com") || url == "deviantart.com"),
        None => Ok(false),
    }
}

//TODO: cache results
pub async fn deviantart_scrape(
    config: &Configuration,
    url: &Url,
    _db: &sled::Db,
) -> Result<Option<ScrapeResult>> {
    let client = crate::scraper::client(config)?;
    let resp = client
        .get(url.to_owned())
        .send()
        .await
        .context("image request failed")?;
    let body = resp.text().await.context("could not read response")?;
    let extract_data = extract_data(config, &body).await?;

    match extract_data {
        None => Ok(None),
        Some((extract_data, camo)) => match extract_data {
            ScrapeResult::Ok(mut v) => {
                let images = try_new_hires(v.images).await?;
                let images = try_intermediary_hires(config, images).await?;
                let source_url = match &v.source_url {
                    Some(v) => v,
                    None => anyhow::bail!("had no source url"),
                };
                let source_url = Url::parse(&crate::scraper::url_to_str(source_url))?;
                let images = try_old_hires(config, source_url, images, &camo).await?;

                v.images = images;

                Ok(Some(ScrapeResult::Ok(v.clone())))
            }
            ScrapeResult::None => Ok(None),
            ScrapeResult::Err(v) => Ok(Some(ScrapeResult::Err(v))),
        },
    }
}

async fn extract_data(config: &Configuration, body: &str) -> Result<Option<(ScrapeResult, Url)>> {
    let image = &IMAGE_REGEX.captures(body);
    let image = match image {
        None => anyhow::bail!("no image found"),
        Some(image) => &image[1],
    };
    let source = &SOURCE_REGEX.captures(body);
    let source = match source {
        None => anyhow::bail!("no source found"),
        Some(source) => &source[1],
    };
    let artist = &ARTIST_REGEX.captures(source);
    let artist = match artist {
        None => anyhow::bail!("no artist found"),
        Some(artist) => &artist[1],
    };
    trace!("deviant capture: {} {} {}", image, source, artist);

    let camo = crate::camo::camo_url(config, &Url::parse(image)?)?;

    trace!("camo_url: {}", camo);

    Ok(Some((
        ScrapeResult::Ok(ScrapeResultData {
            source_url: Some(crate::scraper::from_url(Url::parse(source)?)),
            author_name: Some(artist.to_string()),
            description: None,
            images: vec![ScrapeImage {
                url: crate::scraper::from_url(Url::parse(image)?),
                camo_url: crate::scraper::from_url(camo.clone()),
            }],
        }),
        camo,
    )))
}

async fn try_intermediary_hires(
    config: &Configuration,
    mut images: Vec<ScrapeImage>,
) -> Result<Vec<ScrapeImage>> {
    for image in images.clone() {
        let (domain, object_uuid, object_name) = {
            let caps = CDNINT_REGEX.captures(image.url.as_str());
            let caps = match caps {
                None => continue,
                Some(caps) => caps,
            };
            let domain: &str = &caps[1];
            let object_uuid: &str = &caps[2];
            let object_name: &str = &caps[3];
            (
                domain.to_string(),
                object_uuid.to_string(),
                object_name.to_string(),
            )
        };
        let built_url = format!(
            "{domain}/intermediary/{object_uuid}/{object_name}",
            domain = domain,
            object_uuid = object_uuid,
            object_name = object_name
        );
        let built_url = Url::from_str(&built_url)?;
        let client = client(config)?;
        if client.head(built_url.clone()).send().await?.status() == 200 {
            let built_url = from_url(built_url);
            images.push(ScrapeImage {
                url: built_url,
                camo_url: image.camo_url,
            })
        }
    }
    Ok(images)
}

async fn try_new_hires(mut images: Vec<ScrapeImage>) -> Result<Vec<ScrapeImage>> {
    for image in images.clone() {
        let old_url = image.url.to_string();
        if PNG_REGEX.is_match(&old_url) {
            let new_url = PNG_REGEX.replace(&old_url, |caps: &Captures| {
                format!("{}.png{}", &caps[1], &caps[3])
            });
            let new_url = Url::from_str(&new_url)?;
            images.push(ScrapeImage {
                url: from_url(new_url),
                camo_url: image.camo_url.clone(),
            })
        }
        if JPG_REGEX.is_match(&old_url) {
            let new_url = JPG_REGEX.replace(&old_url, |caps: &Captures| {
                format!("{}100{}", &caps[1], &caps[3])
            });
            let new_url = Url::from_str(&new_url)?;
            images.push(ScrapeImage {
                url: from_url(new_url),
                camo_url: image.camo_url.clone(),
            })
        }
    }
    Ok(images)
}

async fn try_old_hires(
    config: &Configuration,
    source_url: Url,
    mut images: Vec<ScrapeImage>,
    camo: &Url,
) -> Result<Vec<ScrapeImage>> {
    let serial = &SERIAL_REGEX.captures(source_url.as_str());
    let serial = match serial {
        None => anyhow::bail!("no serial captured"),
        Some(serial) => &serial[1],
    };
    let base36 = radix_fmt::radix(serial.parse::<i64>()?, 36)
        .to_string()
        .to_lowercase();

    let built_url = format!(
        "http://orig01.deviantart.net/x_by_x-d{base36}.png",
        base36 = base36
    );

    let client =
        crate::scraper::client_with_redir_limit(config, reqwest::redirect::Policy::none())?;
    let resp = client
        .get(built_url)
        .send()
        .await
        .context("old hires request failed")?;
    if let Some((_, loc)) = resp
        .headers()
        .iter()
        .find(|(name, _value)| name.as_str().to_lowercase() == "location")
    {
        let loc = loc.to_str()?;
        images.push(ScrapeImage {
            url: crate::scraper::from_url(Url::parse(loc)?),
            camo_url: crate::scraper::from_url(camo.clone()),
        });
        return Ok(images);
    }
    return Ok(images);
}
