use std::collections::HashMap;
use std::str::FromStr;

use anyhow::Result;
use futures_cache::{Cache, Duration};
use graphql_client::{GraphQLQuery, Response};
use log::*;
use regex::Regex;
use reqwest::{Client, Url};

use crate::camo::camo_url;
use crate::scraper::{from_url, ScrapeImage, ScrapeResult, ScrapeResultData};
use crate::Configuration;

lazy_static::lazy_static! {
    static ref URL_REGEX: Regex = Regex::from_str(r#"https?://buzzly\.art/~(.*)/art/(.*)"#).unwrap();
}

#[derive(graphql_client::GraphQLQuery)]
#[graphql(
    schema_path = "src/scraper/buzzly/schema.json",
    query_path = "src/scraper/buzzly/query.graphql",
    response_derives = "Debug, Serialize"
)]
pub struct GetSubmission;

pub async fn is_buzzlyart(url: &Url) -> Result<bool> {
    trace!("buzzly on {:?}?", url.as_str());
    if URL_REGEX.is_match_at(url.as_str(), 0) {
        trace!("buzzly matched on URL");
        return Ok(true);
    }
    Ok(false)
}

pub async fn make_buzzly_doc_request(
    client: &Client,
    slug: &str,
    username: &str,
) -> Result<get_submission::ResponseData> {
    #[derive(serde::Serialize)]
    struct Query {
        #[serde(rename = "operationName")]
        operation_name: String,
        query: String,
        variables: HashMap<String, String>,
    }
    let vars = get_submission::Variables {
        slug: slug.to_string(),
        username: username.to_string(),
    };
    let query = GetSubmission::build_query(vars);
    trace!("sending buzzly query {:?}", serde_json::to_string(&query)?);
    let r: Response<get_submission::ResponseData> = client
        .post("https://graphql.buzzly.art/graphql")
        .header("Accept", "application/json")
        .header("Content-Type", "application/json")
        .json(&query)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    Ok(r.data.expect("missing response data"))
}

pub async fn buzzlyart_scrape(
    config: &Configuration,
    url: &Url,
    db: &sled::Db,
) -> Result<Option<ScrapeResult>> {
    trace!("loading buzzly");
    let origin_url = url;
    let reqwest_cache = Cache::load(db.open_tree("buzzly_request_cache")?)?;
    let client = crate::scraper::client(config)?;
    let matches = URL_REGEX.captures(url.as_str()).unwrap();
    let author_name = matches.get(1).unwrap().as_str();
    let slug = matches.get(2).unwrap().as_str();
    let data: get_submission::ResponseData = reqwest_cache
        .wrap(
            (&url, "buzzlyart:requests"),
            Duration::seconds(config.cache_http_duration as i64),
            make_buzzly_doc_request(&client, slug, author_name),
        )
        .await?;
    let data = data
        .fetch_submission_by_username_and_slug
        .ok_or_else(|| anyhow::format_err!("missing data in response"))?;
    let submission = data
        .submission
        .ok_or_else(|| anyhow::format_err!("missing submission metadata"))?;
    let account = submission
        .account
        .as_ref()
        .ok_or_else(|| anyhow::format_err!("missing account metadata"))?;
    trace!("got data: {account:?} {submission:?}");
    let author_name = account.username.clone();
    let description = submission.description;
    let url = submission
        .path
        .ok_or_else(|| anyhow::format_err!("missing image path"))?;
    let camod_url = submission
        .thumbnail_path
        .ok_or_else(|| anyhow::format_err!("missing image path"))?;
    let url: url::Url = Url::from_str(&format!("https://submissions.buzzly.art{url}"))?;
    let camod_url = Url::from_str(&format!("https://submissions.buzzly.art{camod_url}"))?;
    let tags: Vec<Option<String>> = submission
        .tags
        .ok_or_else(|| anyhow::format_err!("missing tags fields"))?;
    let mut tags: Vec<String> = tags.into_iter().flatten().collect();
    tags.push(format!("artist:{author_name}"));
    Ok(Some(ScrapeResult::Ok(ScrapeResultData {
        source_url: Some(from_url(origin_url.clone())),
        author_name: Some(author_name),
        additional_tags: Some(tags),
        description: Some(description),
        images: vec![ScrapeImage {
            url: from_url(url),
            camo_url: from_url(camo_url(config, &camod_url)?),
        }],
    })))
}

#[cfg(test)]
mod test {
    use crate::scraper::{scrape, ScrapeResultData};

    use super::*;

    #[test]
    fn test_buzzlyart_scraper() -> Result<()> {
        crate::LOGGER.lock().unwrap().flush();
        let url = r#"https://buzzly.art/~mothnmag/art/fizzy"#;
        let config = Configuration::default();
        let db = sled::Config::default().temporary(true).open()?;
        let scrape = tokio_test::block_on(scrape(&config, &db, url))?.unwrap();

        visit_diff::assert_eq_diff!(ScrapeResult::Ok(ScrapeResultData{
            source_url: Some(
                "https://buzzly.art/~mothnmag/art/fizzy".to_string(),
            ),
            author_name: Some(
                "mothnmag".to_string(),
            ),
            additional_tags: Some(
                vec![
                    "mlp".to_string(),
                    "g1".to_string(),
                    "mlpg1".to_string(),
                    "fizzy".to_string(),
                    "artist:mothnmag".to_string(),
                ],
            ),
            description: Some(
                "<p>AHH sorry i havent posted in a while work has been so busy h</p><p>but!! heres some fizzy art for oskar :3</p>".to_string(),
            ),
            images: vec![
                ScrapeImage {
                    url: "https://submissions.buzzly.art/IMAGE/542f4f12-a882-4899-b37e-e4fd0e1765d4_055d6284-907c-4f84-a99b-2502201f4100.png".to_string(),
                    camo_url: "https://submissions.buzzly.art/IMAGE/542f4f12-a882-4899-b37e-e4fd0e1765d4_67a9175f-04c3-4401-961a-670cc10c6a08_thumbnail.webp".to_string(),
                },
            ],
        }), scrape);

        Ok(())
    }
}
