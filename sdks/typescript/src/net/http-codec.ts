/**
 * HTTP/1.1 on the guest's side of the wire, `fetch` on ours.
 */

const CR = 0x0d;
const LF = 0x0a;

const decoder = new TextDecoder();
const encoder = new TextEncoder();

export const FORBIDDEN_REQUEST_HEADERS = new Set([
    "accept-charset",
    "accept-encoding",
    "access-control-request-headers",
    "access-control-request-method",
    "connection",
    "content-length",
    "cookie",
    "cookie2",
    "date",
    "dnt",
    "expect",
    "host",
    "keep-alive",
    "origin",
    "referer",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
    "via",
]);

function isForbidden(name: string): boolean {
    const lower = name.toLowerCase();
    return (
        FORBIDDEN_REQUEST_HEADERS.has(lower) ||
        lower.startsWith("proxy-") ||
        lower.startsWith("sec-")
    );
}

export interface ParsedRequest {
    method: string;
    target: string;
    headers: [string, string][];
    body: Uint8Array | undefined;
    keepAlive: boolean;
    consumed: number;
}

export type RequestResult =
    | { kind: "ok"; request: ParsedRequest }
    | { kind: "incomplete" }
    | { kind: "invalid"; reason: string };

function findHeaderEnd(bytes: Uint8Array): number {
    for (let i = 3; i < bytes.length; i++) {
        if (
            bytes[i] === LF &&
            bytes[i - 1] === CR &&
            bytes[i - 2] === LF &&
            bytes[i - 3] === CR
        ) {
            return i + 1;
        }
    }
    return -1;
}

function headerValue(headers: [string, string][], name: string): string | undefined {
    const lower = name.toLowerCase();
    for (const [key, value] of headers) {
        if (key.toLowerCase() === lower) {
            return value;
        }
    }
    return undefined;
}

const REQUEST_HEADER_MAX_BYTES = 65536;

export function parseRequest(bytes: Uint8Array): RequestResult {
    const headerEnd = findHeaderEnd(bytes);
    if (headerEnd === -1) {
        if (bytes.length > REQUEST_HEADER_MAX_BYTES) {
            return {
                kind: "invalid",
                reason: `no end of headers in ${bytes.length} bytes, this transport carries HTTP/1.x only`,
            };
        }
        return { kind: "incomplete" };
    }

    const lines = decoder.decode(bytes.subarray(0, headerEnd)).split("\r\n");
    const requestLine = lines[0] ?? "";
    const fields = requestLine.split(" ");

    if (fields.length !== 3) {
        return { kind: "invalid", reason: `malformed request line: ${requestLine.slice(0, 60)}` };
    }

    const [method, target, version] = fields as [string, string, string];
    if (!version.startsWith("HTTP/1.")) {
        return { kind: "invalid", reason: `unsupported version ${version}` };
    }

    const headers: [string, string][] = [];
    for (const line of lines.slice(1)) {
        if (line === "") {
            continue;
        }

        const colon = line.indexOf(":");
        if (colon === -1) {
            return { kind: "invalid", reason: `malformed header line: ${line.slice(0, 60)}` };
        }

        headers.push([line.slice(0, colon).trim(), line.slice(colon + 1).trim()]);
    }

    const connection = headerValue(headers, "connection")?.toLowerCase();
    const keepAlive =
        connection === "close" ? false : version === "HTTP/1.1" || connection === "keep-alive";

    const transferEncoding = headerValue(headers, "transfer-encoding")?.toLowerCase();
    if (transferEncoding !== undefined && transferEncoding.includes("chunked")) {
        const chunked = readChunkedBody(bytes, headerEnd);
        if (chunked === undefined) {
            return { kind: "incomplete" };
        }
        return {
            kind: "ok",
            request: {
                method,
                target,
                headers,
                body: chunked.body,
                keepAlive,
                consumed: chunked.consumed,
            },
        };
    }

    const declaredLength = headerValue(headers, "content-length");
    const length = declaredLength === undefined ? 0 : Number(declaredLength);

    if (!Number.isInteger(length) || length < 0) {
        return { kind: "invalid", reason: `bad Content-Length ${declaredLength}` };
    }

    if (bytes.length < headerEnd + length) {
        return { kind: "incomplete" };
    }

    return {
        kind: "ok",
        request: {
            method,
            target,
            headers,
            body: length === 0 ? undefined : bytes.slice(headerEnd, headerEnd + length),
            keepAlive,
            consumed: headerEnd + length,
        },
    };
}

function readChunkedBody(
    bytes: Uint8Array,
    start: number,
): { body: Uint8Array | undefined; consumed: number } | undefined {
    const pieces: Uint8Array[] = [];
    let cursor = start;

    for (;;) {
        let lineEnd = -1;
        for (let i = cursor; i + 1 < bytes.length; i++) {
            if (bytes[i] === CR && bytes[i + 1] === LF) {
                lineEnd = i;
                break;
            }
        }

        if (lineEnd === -1) {
            return undefined;
        }

        const sizeText = decoder.decode(bytes.subarray(cursor, lineEnd)).split(";")[0] ?? "";
        const size = Number.parseInt(sizeText.trim(), 16);
        if (Number.isNaN(size) || size < 0) {
            return undefined;
        }

        const dataStart = lineEnd + 2;

        if (size === 0) {
            const end = findHeaderEnd(bytes.subarray(cursor));
            if (end === -1) {
                if (bytes.length < dataStart + 2) {
                    return undefined;
                }
                return { body: joinPieces(pieces), consumed: dataStart + 2 };
            }
            return { body: joinPieces(pieces), consumed: cursor + end };
        }

        if (bytes.length < dataStart + size + 2) {
            return undefined;
        }

        pieces.push(bytes.slice(dataStart, dataStart + size));
        cursor = dataStart + size + 2;
    }
}

function joinPieces(pieces: Uint8Array[]): Uint8Array | undefined {
    if (pieces.length === 0) {
        return undefined;
    }

    const total = pieces.reduce((sum, piece) => sum + piece.length, 0);
    const joined = new Uint8Array(total);
    let offset = 0;
    for (const piece of pieces) {
        joined.set(piece, offset);
        offset += piece.length;
    }

    return joined;
}

export interface FetchableRequest {
    url: string;
    method: string;
    headers: [string, string][];
    body: Uint8Array | undefined;
    stripped: string[];
}

export function toFetchable(
    request: ParsedRequest,
    host: string,
    port: number,
    secure: boolean,
): FetchableRequest {
    const scheme = secure ? "https" : "http";
    const authority = port === (secure ? 443 : 80) ? host : `${host}:${port}`;

    const url = /^https?:\/\//i.test(request.target)
        ? request.target
        : `${scheme}://${authority}${request.target}`;

    const headers: [string, string][] = [];
    const stripped: string[] = [];

    for (const [name, value] of request.headers) {
        if (isForbidden(name)) {
            stripped.push(name);
            continue;
        }
        headers.push([name, value]);
    }

    return { url, method: request.method, headers, body: request.body, stripped };
}

export function serializeResponse(
    status: number,
    statusText: string,
    headers: [string, string][],
    body: Uint8Array,
    keepAlive: boolean,
): Uint8Array {
    const lines = [`HTTP/1.1 ${status} ${statusText || reasonFor(status)}`];

    for (const [name, value] of headers) {
        const lower = name.toLowerCase();

        if (
            lower === "content-length" ||
            lower === "transfer-encoding" ||
            lower === "connection" ||
            lower === "content-encoding"
        ) {
            continue;
        }
        lines.push(`${name}: ${value}`);
    }

    lines.push(`Content-Length: ${body.length}`);
    lines.push(`Connection: ${keepAlive ? "keep-alive" : "close"}`);
    lines.push("", "");

    const head = encoder.encode(lines.join("\r\n"));
    const out = new Uint8Array(head.length + body.length);
    out.set(head, 0);
    out.set(body, head.length);

    return out;
}

const REASONS = new Map([
    [200, "OK"],
    [201, "Created"],
    [204, "No Content"],
    [301, "Moved Permanently"],
    [302, "Found"],
    [304, "Not Modified"],
    [307, "Temporary Redirect"],
    [308, "Permanent Redirect"],
    [400, "Bad Request"],
    [401, "Unauthorized"],
    [403, "Forbidden"],
    [404, "Not Found"],
    [500, "Internal Server Error"],
    [502, "Bad Gateway"],
    [503, "Service Unavailable"],
]);

function reasonFor(status: number): string {
    return REASONS.get(status) ?? "Unknown";
}

export function serializeTransportError(reason: string): Uint8Array {
    const body = encoder.encode(`vpod: ${reason}\n`);
    return serializeResponse(502, "Bad Gateway", [["Content-Type", "text/plain"]], body, false);
}
