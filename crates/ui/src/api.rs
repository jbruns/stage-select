//! Requests to the Service's `/api/v1`, same-origin, so the trusted proxy's identity header or the loopback
//! dev identity authenticates them and no token is kept in the browser.

use std::sync::OnceLock;

use gloo_net::http::{Request, Response};
use leptos::prelude::{ArcRwSignal, Update};
use serde::Serialize;
use serde::de::DeserializeOwned;
use stage_select_api::{ErrorBody, GameRow, PREFIX, Page};

pub fn url(path: &str) -> String {
    format!("{PREFIX}{path}")
}

/// Escapes a name, such as a Source Set's, for use as one path segment.
pub fn segment(name: &str) -> String {
    String::from(js_sys::encode_uri_component(name))
}

async fn checked(response: Response) -> Result<Response, String> {
    refused(response).await.map_err(|body| body.error)
}

/// The response, or the Service's refusal with the Job it names, if any.
async fn refused(response: Response) -> Result<Response, ErrorBody> {
    if response.ok() {
        return Ok(response);
    }
    let status = response.status();
    match response.json::<ErrorBody>().await {
        Ok(body) => Err(body),
        Err(_) => Err(ErrorBody {
            error: format!("The service answered {status}."),
            job: None,
            blockers: None,
        }),
    }
}

/// Bumped after every write the Service accepted, whatever it changed. Some writes change a status without a Job, such
/// as proposing a repair or opening a plan, so whatever counts statuses outside the screen that wrote, like the
/// navigation badges, fetches again on it as well as on settled Jobs.
pub fn writes() -> ArcRwSignal<u64> {
    static WRITES: OnceLock<ArcRwSignal<u64>> = OnceLock::new();
    WRITES.get_or_init(|| ArcRwSignal::new(0)).clone()
}

fn wrote<T, E>(result: Result<T, E>) -> Result<T, E> {
    if result.is_ok() {
        writes().update(|n| *n += 1);
    }
    result
}

fn failed(err: gloo_net::Error) -> String {
    format!("The service could not be reached: {err}")
}

pub async fn get<T: DeserializeOwned>(path: &str) -> Result<T, String> {
    let response = Request::get(&url(path)).send().await.map_err(failed)?;
    checked(response)
        .await?
        .json()
        .await
        .map_err(|e| e.to_string())
}

/// Like `get`, but `None` when the Service finds nothing there (404).
pub async fn get_found<T: DeserializeOwned>(path: &str) -> Result<Option<T>, String> {
    let response = Request::get(&url(path)).send().await.map_err(failed)?;
    if response.status() == 404 {
        return Ok(None);
    }
    checked(response)
        .await?
        .json()
        .await
        .map(Some)
        .map_err(|e| e.to_string())
}

pub async fn text(path: &str) -> Result<String, String> {
    let response = Request::get(&url(path)).send().await.map_err(failed)?;
    checked(response)
        .await?
        .text()
        .await
        .map_err(|e| e.to_string())
}

pub async fn post<B: Serialize, T: DeserializeOwned>(path: &str, body: &B) -> Result<T, String> {
    post_until(path, body, None).await
}

/// Like `post`, but a refusal keeps the Job it names, such as the one already doing the work.
pub async fn post_refusable<B: Serialize, T: DeserializeOwned>(
    path: &str,
    body: &B,
) -> Result<T, ErrorBody> {
    let plain = |error: String| ErrorBody {
        error,
        job: None,
        blockers: None,
    };
    let request = Request::post(&url(path))
        .json(body)
        .map_err(|e| plain(e.to_string()))?;
    let response = request.send().await.map_err(|e| plain(failed(e)))?;
    wrote(refused(response).await)?
        .json()
        .await
        .map_err(|e| plain(e.to_string()))
}

/// Like `post`, but `signal` aborts the request, which closes the connection so the Service stops the work.
pub async fn post_until<B: Serialize, T: DeserializeOwned>(
    path: &str,
    body: &B,
    signal: Option<&web_sys::AbortSignal>,
) -> Result<T, String> {
    let request = Request::post(&url(path))
        .abort_signal(signal)
        .json(body)
        .map_err(|e| e.to_string())?;
    let response = request.send().await.map_err(failed)?;
    wrote(checked(response).await)?
        .json()
        .await
        .map_err(|e| e.to_string())
}

/// Like `post`, for a request the Service answers with no body.
pub async fn post_empty<B: Serialize>(path: &str, body: &B) -> Result<(), String> {
    let request = Request::post(&url(path))
        .json(body)
        .map_err(|e| e.to_string())?;
    let response = request.send().await.map_err(failed)?;
    wrote(checked(response).await).map(drop)
}

pub async fn patch<B: Serialize, T: DeserializeOwned>(path: &str, body: &B) -> Result<T, String> {
    let request = Request::patch(&url(path))
        .json(body)
        .map_err(|e| e.to_string())?;
    let response = request.send().await.map_err(failed)?;
    wrote(checked(response).await)?
        .json()
        .await
        .map_err(|e| e.to_string())
}

pub async fn put<B: Serialize, T: DeserializeOwned>(path: &str, body: &B) -> Result<T, String> {
    let request = Request::put(&url(path))
        .json(body)
        .map_err(|e| e.to_string())?;
    let response = request.send().await.map_err(failed)?;
    wrote(checked(response).await)?
        .json()
        .await
        .map_err(|e| e.to_string())
}

pub async fn delete<T: DeserializeOwned>(path: &str) -> Result<T, String> {
    let response = Request::delete(&url(path)).send().await.map_err(failed)?;
    wrote(checked(response).await)?
        .json()
        .await
        .map_err(|e| e.to_string())
}

pub async fn post_form<T: DeserializeOwned>(
    path: &str,
    form: web_sys::FormData,
) -> Result<T, String> {
    let request = Request::post(&url(path))
        .body(form)
        .map_err(|e| e.to_string())?;
    let response = request.send().await.map_err(failed)?;
    wrote(checked(response).await)?
        .json()
        .await
        .map_err(|e| e.to_string())
}

/// Every Game Status of an Audit, a page of 1000 at a time.
pub async fn games(audit: i64) -> Result<Vec<GameRow>, String> {
    let mut games = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let mut path = format!("/audits/{audit}/games?limit=1000");
        if let Some(cursor) = &cursor {
            path.push_str("&cursor=");
            path.push_str(&segment(cursor));
        }
        let page: Page<GameRow> = get(&path).await?;
        games.extend(page.items);
        match page.next_cursor {
            Some(next) => cursor = Some(next),
            None => return Ok(games),
        }
    }
}
