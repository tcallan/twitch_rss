use core::fmt;
use std::env;
use std::io::Cursor;

use cached::proc_macro::cached;
use reqwest::{Client as ReqwestClient, StatusCode};
use rocket::http::Status;
use rocket::response::content::Xml;
use rocket::response::Responder;
use rocket::{get, launch, routes, Response, State};
use rss::{ChannelBuilder, GuidBuilder, Item, ItemBuilder};
use twitch_api2::helix::videos::{get_videos, Video};
use twitch_api2::helix::{ClientRequestError, HelixClient, HelixRequestGetError};
use twitch_api2::twitch_oauth2::{AppAccessToken, ClientId, ClientSecret};
use twitch_api2::types::{Nickname, UserId};

#[derive(Debug)]
enum TwitchRssError {
    Token(String),
    UnknownChannel(String),
    Unauthorized,
    RequestError(String),
    FeedBuild(String),
}

impl fmt::Display for TwitchRssError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self {
            Self::Token(e) => write!(f, "Token({})", e),
            Self::UnknownChannel(ch) => write!(f, "UnknownChannel({})", ch),
            Self::Unauthorized => write!(f, "Unauthorized"),
            Self::RequestError(e) => write!(f, "RequestError({})", e),
            Self::FeedBuild(e) => write!(f, "FeedBuild({})", e),
        }
    }
}

impl std::error::Error for TwitchRssError {}

impl<'r> Responder<'r, 'static> for TwitchRssError {
    fn respond_to(self, _: &'r rocket::Request<'_>) -> rocket::response::Result<'static> {
        let err_string = format!("{}", self);
        let status = match self {
            Self::Token(_) => Status::InternalServerError,
            Self::UnknownChannel(_) => Status::NotFound,
            Self::Unauthorized => Status::InternalServerError,
            Self::RequestError(_) => Status::InternalServerError,
            Self::FeedBuild(_) => Status::InternalServerError,
        };
        Response::build()
            .sized_body(err_string.len(), Cursor::new(err_string))
            .status(status)
            .ok()
    }
}

#[get("/<name>/id")]
async fn world(
    name: &str,
    client: &State<ReqwestClient>,
    client_id: &State<ClientId>,
    client_secret: &State<ClientSecret>,
) -> Result<String, TwitchRssError> {
    let token = get_token(
        client.inner(),
        client_id.inner().clone(),
        client_secret.inner().clone(),
    )
    .await?;

    let helix_client = HelixClient::with_client(client.inner().clone());

    let user_id = get_user_id(&helix_client, &token, name.into()).await?;

    Ok(format!("{}", user_id))
}

#[get("/<name>/vod")]
async fn channel(
    name: &str,
    client: &State<ReqwestClient>,
    client_id: &State<ClientId>,
    client_secret: &State<ClientSecret>,
) -> Result<Xml<String>, TwitchRssError> {
    let token = get_token(
        client.inner(),
        client_id.inner().clone(),
        client_secret.inner().clone(),
    )
    .await?;

    let helix_client = HelixClient::with_client(client.inner().clone());

    let user_id = get_user_id(&helix_client, &token, name.into()).await?;

    let videos = get_user_videos(&helix_client, &token, user_id).await?;

    let items = videos
        .iter()
        .map(video_to_rss_item)
        .collect::<Result<Vec<_>, TwitchRssError>>()?;

    let feed = ChannelBuilder::default()
        .title(format!("{} Twitch VODs", name))
        .items(items)
        .build()
        .map_err(handle_feed_error)?
        .to_string();

    Ok(Xml(feed))
}

#[launch]
fn rocket() -> _ {
    let client = ReqwestClient::new();
    let client_id: ClientId = env::var("TWITCH_CLIENT_ID")
        .expect("TWITCH_CLIENT_ID is not set")
        .into();
    let client_secret: ClientSecret = env::var("TWITCH_CLIENT_SECRET")
        .expect("TWITCH_CLIENT_SECRET is not set")
        .into();

    rocket::build()
        .manage(client)
        .manage(client_id)
        .manage(client_secret)
        .mount("/channel", routes![world, channel])
}

fn handle_feed_error(err: String) -> TwitchRssError {
    TwitchRssError::FeedBuild(err)
}

fn video_to_rss_item(input: &Video) -> Result<Item, TwitchRssError> {
    let guid = GuidBuilder::default()
        .value(input.id.to_string())
        .build()
        .map_err(handle_feed_error)?;

    let published = input.created_at.to_utc().to_rfc2822();

    let thumbnail_url = input
        .thumbnail_url
        .replace("%{width}", "512")
        .replace("%{height}", "288");

    let description = format!(
        "<a href=\"{}\"><img src=\"{}\" /></a><br />{}",
        input.url, thumbnail_url, input.description
    );

    ItemBuilder::default()
        .guid(guid)
        .pub_date(published)
        .title(input.title.clone())
        .link(input.url.clone())
        .description(description)
        .build()
        .map_err(handle_feed_error)
}

fn handle_helix_error(err: ClientRequestError<reqwest::Error>) -> TwitchRssError {
    match err {
        ClientRequestError::HelixRequestGetError(HelixRequestGetError::Error {
            status, ..
        }) if status == StatusCode::UNAUTHORIZED => TwitchRssError::Unauthorized,
        e => TwitchRssError::RequestError(format!("{}", e)),
    }
}

#[cached(
    time = 1200,
    result = true,
    key = "(ClientId, ClientSecret)",
    convert = "{ (client_id.clone(), client_secret.clone()) }"
)]
async fn get_token(
    client: &ReqwestClient,
    client_id: ClientId,
    client_secret: ClientSecret,
) -> Result<AppAccessToken, TwitchRssError> {
    println!("getting token");
    match AppAccessToken::get_app_access_token(client, client_id, client_secret, vec![]).await {
        Ok(t) => Ok(t),
        Err(e) => {
            println!("{:?}", e);
            Err(TwitchRssError::Token(format!("{}", e)))
        }
    }
}

#[cached(
    time = 600,
    result = true,
    key = "Nickname",
    convert = "{ user_name.clone() }"
)]
async fn get_user_id(
    client: &HelixClient<'static, ReqwestClient>,
    token: &AppAccessToken,
    user_name: Nickname,
) -> Result<UserId, TwitchRssError> {
    println!("getting user {}", user_name);
    let maybe_channel = client
        .get_channel_from_login(user_name.clone(), token)
        .await
        .map_err(handle_helix_error)?;

    maybe_channel
        .map(|c| c.broadcaster_id)
        .ok_or(TwitchRssError::UnknownChannel(user_name.to_string()))
}

#[cached(
    time = 600,
    result = true,
    key = "UserId",
    convert = "{ user_id.clone() }"
)]
async fn get_user_videos(
    client: &HelixClient<'static, ReqwestClient>,
    token: &AppAccessToken,
    user_id: UserId,
) -> Result<Vec<Video>, TwitchRssError> {
    println!("getting videos for {}", user_id);
    let video_request = get_videos::GetVideosRequest::builder()
        .user_id(user_id)
        .build();

    let videos = client
        .req_get(video_request, token)
        .await
        .map_err(handle_helix_error)?
        .data;

    Ok(videos)
}
