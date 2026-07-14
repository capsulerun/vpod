use crate::exports::vpod::sandbox::executor::{HttpHeader, HttpResponse};

use wasi::http::outgoing_handler;
use wasi::http::types::{Fields, Method, OutgoingBody, OutgoingRequest, Scheme};
use wasi::io::streams::StreamError;

const READ_CHUNK: u64 = 64 * 1024;

fn split_url(url: &str) -> Result<(Scheme, String, String), String> {
    let (scheme, rest) = if let Some(rest) = url.strip_prefix("https://") {
        (Scheme::Https, rest)
    } else if let Some(rest) = url.strip_prefix("http://") {
        (Scheme::Http, rest)
    } else {
        return Err(format!("unsupported URL scheme: {url}"));
    };

    let (authority, path) = match rest.find('/') {
        Some(index) => (&rest[..index], &rest[index..]),
        None => (rest, "/"),
    };

    if authority.is_empty() {
        return Err(format!("missing host in URL: {url}"));
    }

    Ok((scheme, authority.to_string(), path.to_string()))
}

fn method_from_str(method: &str) -> Method {
    match method.to_ascii_uppercase().as_str() {
        "GET" => Method::Get,
        "HEAD" => Method::Head,
        "POST" => Method::Post,
        "PUT" => Method::Put,
        "DELETE" => Method::Delete,
        "PATCH" => Method::Patch,
        "OPTIONS" => Method::Options,
        other => Method::Other(other.to_string()),
    }
}

pub fn fetch(
    method: String,
    url: String,
    headers: Vec<HttpHeader>,
    body: Option<Vec<u8>>,
) -> Result<HttpResponse, String> {
    let (scheme, authority, path) = split_url(&url)?;

    let header_pairs: Vec<(String, Vec<u8>)> = headers
        .into_iter()
        .map(|h| (h.name, h.value.into_bytes()))
        .collect();
    let fields =
        Fields::from_list(&header_pairs).map_err(|e| format!("invalid request headers: {e:?}"))?;

    let request = OutgoingRequest::new(fields);
    request
        .set_method(&method_from_str(&method))
        .map_err(|_| "invalid HTTP method".to_string())?;
    request
        .set_scheme(Some(&scheme))
        .map_err(|_| "invalid scheme".to_string())?;
    request
        .set_authority(Some(&authority))
        .map_err(|_| "invalid authority".to_string())?;
    request
        .set_path_with_query(Some(&path))
        .map_err(|_| "invalid path".to_string())?;

    if let Some(bytes) = body {
        write_request_body(&request, &bytes)?;
    }

    let future =
        outgoing_handler::handle(request, None).map_err(|e| format!("request failed: {e:?}"))?;

    let pollable = future.subscribe();
    let response = loop {
        if let Some(result) = future.get() {
            break result
                .map_err(|_| "response already consumed".to_string())?
                .map_err(|e| format!("http error: {e:?}"))?;
        }
        pollable.block();
    };

    let status = response.status();
    let response_headers = response
        .headers()
        .entries()
        .into_iter()
        .map(|(name, value)| HttpHeader {
            name,
            value: String::from_utf8_lossy(&value).into_owned(),
        })
        .collect();

    let body = read_response_body(response)?;

    Ok(HttpResponse {
        status,
        headers: response_headers,
        body,
    })
}

fn write_request_body(request: &OutgoingRequest, bytes: &[u8]) -> Result<(), String> {
    let outgoing_body = request
        .body()
        .map_err(|_| "request body already taken".to_string())?;

    {
        let stream = outgoing_body
            .write()
            .map_err(|_| "request body stream already taken".to_string())?;

        for chunk in bytes.chunks(READ_CHUNK as usize) {
            stream
                .blocking_write_and_flush(chunk)
                .map_err(|e| format!("failed to write request body: {e:?}"))?;
        }
    }

    OutgoingBody::finish(outgoing_body, None)
        .map_err(|e| format!("failed to finish request body: {e:?}"))?;

    Ok(())
}

fn read_response_body(response: wasi::http::types::IncomingResponse) -> Result<Vec<u8>, String> {
    let incoming_body = response
        .consume()
        .map_err(|_| "response body already consumed".to_string())?;

    let mut data = Vec::new();
    {
        let stream = incoming_body
            .stream()
            .map_err(|_| "response body stream already taken".to_string())?;

        loop {
            match stream.blocking_read(READ_CHUNK) {
                Ok(chunk) => data.extend_from_slice(&chunk),
                Err(StreamError::Closed) => break,
                Err(e) => return Err(format!("failed to read response body: {e:?}")),
            }
        }
    }

    Ok(data)
}
