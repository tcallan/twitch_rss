use core::fmt;
use std::env;
use std::net::SocketAddr;

use axum::extract::{Path, State};
use axum::response::IntoResponse;
use axum::routing::get;
use axum::Router;
use cached::proc_macro::cached;
use reqwest::{Client as ReqwestClient, StatusCode};
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
}

impl fmt::Display for TwitchRssError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self {
            Self::Token(e) => write!(f, "Token({})", e),
            Self::UnknownChannel(ch) => write!(f, "UnknownChannel({})", ch),
            Self::Unauthorized => write!(f, "Unauthorized"),
            Self::RequestError(e) => write!(f, "RequestError({})", e),
        }
    }
}

impl std::error::Error for TwitchRssError {}

impl IntoResponse for TwitchRssError {
    fn into_response(self) -> axum::response::Response {
        let err_string = format!("{}", self);
        let status = match self {
            Self::Token(_) => StatusCode::INTERNAL_SERVER_ERROR,
            Self::UnknownChannel(_) => StatusCode::NOT_FOUND,
            Self::Unauthorized => StatusCode::INTERNAL_SERVER_ERROR,
            Self::RequestError(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };

        (status, err_string).into_response()
    }
}

struct RssXml<T>(T);

impl<T: IntoResponse> IntoResponse for RssXml<T> {
    fn into_response(self) -> axum::response::Response {
        (
            [(axum::http::header::CONTENT_TYPE, "application/rss+xml")],
            self.0,
        )
            .into_response()
    }
}

async fn world(
    Path(name): Path<String>,
    State(state): State<AppState>,
) -> Result<String, TwitchRssError> {
    let token = get_token(
        &state.client,
        state.client_id.clone(),
        state.client_secret.clone(),
    )
    .await?;

    let helix_client = HelixClient::with_client(state.client.clone());

    let user_id = get_user_id(&helix_client, &token, name.into()).await?;

    Ok(format!("{}", user_id))
}

async fn channel(
    Path(name): Path<String>,
    State(state): State<AppState>,
) -> Result<RssXml<String>, TwitchRssError> {
    let token = get_token(
        &state.client,
        state.client_id.clone(),
        state.client_secret.clone(),
    )
    .await?;

    let helix_client = HelixClient::with_client(state.client.clone());

    let user_id = get_user_id(&helix_client, &token, name.clone().into()).await?;

    let videos = get_user_videos(&helix_client, &token, user_id).await?;

    let items = videos.iter().map(video_to_rss_item).collect::<Vec<_>>();

    let feed = ChannelBuilder::default()
        .title(format!("{} Twitch VODs", name))
        .items(items)
        .build()
        .to_string();

    Ok(RssXml(feed))
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let port: u16 = env::var("PORT")
        .expect("PORT is not set")
        .parse()
        .expect("PORT is not a valid value");

    let client = ReqwestClient::new();
    let client_id: ClientId = env::var("TWITCH_CLIENT_ID")
        .expect("TWITCH_CLIENT_ID is not set")
        .into();
    let client_secret: ClientSecret = env::var("TWITCH_CLIENT_SECRET")
        .expect("TWITCH_CLIENT_SECRET is not set")
        .into();

    let channel = Router::new()
        .route("/:name/vod", get(channel))
        .route("/:name/id", get(world));

    let app = Router::new()
        .nest("/channel", channel)
        .with_state(AppState {
            client,
            client_id,
            client_secret,
        });

    let socket = SocketAddr::from(([0, 0, 0, 0], port));
    axum::Server::try_bind(&socket)?
        .serve(app.into_make_service())
        .await?;

    Ok(())
}

#[derive(Clone)]
struct AppState {
    client: ReqwestClient,
    client_id: ClientId,
    client_secret: ClientSecret,
}

fn video_to_rss_item(input: &Video) -> Item {
    let guid = GuidBuilder::default().value(input.id.to_string()).build();

    let published = input.created_at.to_utc().to_rfc2822();

    ItemBuilder::default()
        .guid(guid)
        .pub_date(published)
        .title(input.title.clone())
        .link(input.url.clone())
        .description(build_description(input))
        .build()
}

fn build_description(input: &Video) -> String {
    let thumbnail_url = input
        .thumbnail_url
        .replace("%{width}", "512")
        .replace("%{height}", "288");

    let mut description = format!(
        "<a href=\"{}\"><img src=\"{}\" /></a>",
        input.url, thumbnail_url
    );

    // include twitch video description if it exists
    if !input.description.is_empty() {
        description.push_str("<br />");
        description.push_str(&input.description);
    }

    // include video title for buggy RSS readers that only update if the description itself changes
    description.push_str("<br />");
    description.push_str(&input.title);
    description
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
        .ok_or_else(|| TwitchRssError::UnknownChannel(user_name.to_string()))
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
