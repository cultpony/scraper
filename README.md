# scraper.rs

A scraper intended for use with philomena. This tool presents a simple HTTP endpoint that returns URLs of image files belonging to a social media post.

## Compiling and Install

Simply checkout scraper.rs, and run either "cargo test" or "cargo build".

Then run "cargo test" to verify the scraper is working (requires Tumblr API Key)

"cargo run" will run the current source code, otherwise use "cargo build --release" to generate a release build.

## Configuration

For configuration see `.env.example`.

A Tumblr API Key is required

## Scrapers

Available Scrapers are:

| Service     | Status      | Notes                                                                         |
|-------------|-------------|-------------------------------------------------------------------------------|
| DeviantArt  | Alpha       | Will likely be able to grab atleast the CDN Image, which is usually hi-res    |
| Twitter     | Production  | Requires regular adjustment but works otherwise                               |
| Nitter      | Production  | Only supports officially listed instances                                     |
| Tumblr      | Beta        | Missing Text-Post Scraping                                                    |
| Raw         | Production  | Valid for gif, jpeg, png, svg, webm                                           |
| Philomena   | Production  | Works for a selected number of boorus                                         |
| Buzzly.Art  | Production  | Works, Supports Additional Tags                                               |

## API

Make a request to `<domain>/images/scrape`. Scraper.rs accepts POSTS and optionally GET requests.

For the GET request, simply put an URL encoded query into the query parameter "url". In the POST method, simply encode the request as JSON with the object attribute "url" set.

Example:

```
POST www.example.com/images/scrape
{
    "url": "some-tumblr-blog.com/my-post-id-/image"
}
```

You will receive a scrape response with 200 Status Code if the request is accepted. If the "errors" field is populated, you must ignore the remainder of the object. The errors field is an array containing strings describing the error path.

Example of an error:

```
{"errors":["URL invalid"]}
{"errors":["Twitter parser failed","invalid api response","API request is not 200 code"]}
```

Otherwise, the response will look like this;

```
{
    "source_url":"https://twitter.com/user/status/1000000000000000000",
    "author_name":"user",
    "description":"My tweet\nhas some images I made",
    "images":[
        {
            "url":"https://pbs.twimg.com/media/EpiHor000000000.jpg",
            "camo_url":"https://pbs.twimg.com/media/EpiHor000000000.jpg"
        },
        {
            "url":"https://pbs.twimg.com/media/EpiHor000000001.jpg",
            "camo_url":"https://pbs.twimg.com/media/EpiHor000000001.jpg"
        },
        {
            "url":"https://pbs.twimg.com/media/EpiHor000000002.jpg",
            "camo_url":"https://pbs.twimg.com/media/EpiHor000000002.jpg"
        },
        {
            "url":"https://pbs.twimg.com/media/EpiHor000000003.jpg",
            "camo_url":"https://pbs.twimg.com/media/EpiHor000000003.jpg"
        }
    ]
}
```