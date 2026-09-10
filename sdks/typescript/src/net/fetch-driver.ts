/**
 * Turns the emulator's plaintext HTTP into `fetch` calls.
 */

import { parsePreamble } from "./preamble.js";
import { RingWriter } from "./ring.js";
import {
    parseRequest,
    serializeResponse,
    serializeTransportError,
    toFetchable,
} from "./http-codec.js";
import type { ParsedRequest } from "./http-codec.js";
import type { DriverCommand, DriverOptions } from "./driver-protocol.js";

const DEFAULT_REQUEST_TIMEOUT_MILLISECONDS = 120_000;

interface Connection {
    writer: RingWriter;
    buffered: Uint8Array;
    host: string | undefined;
    resolvedHostname: string | undefined;
    port: number;
    secure: boolean;
    queue: Promise<void>;
    guestFinishedWriting: boolean;
    closed: boolean;
}

function concat(left: Uint8Array, right: Uint8Array): Uint8Array {
    const joined = new Uint8Array(left.length + right.length);
    joined.set(left, 0);
    joined.set(right, left.length);
    return joined;
}

function corsIsEnforced(): boolean {
    const global = globalThis as { window?: unknown; WorkerGlobalScope?: unknown };
    return global.window !== undefined || global.WorkerGlobalScope !== undefined;
}

function pageIsServedOverHttps(): boolean {
    const global = globalThis as { location?: { protocol?: string } };
    return global.location?.protocol === "https:";
}

function browserIsOffline(): boolean {
    const global = globalThis as { navigator?: { onLine?: boolean } };
    return global.navigator?.onLine === false;
}

const BROWSER_BLOCKED_PORTS = new Set([
    1, 7, 9, 11, 13, 15, 17, 19, 20, 21, 22, 23, 25, 37, 42, 43, 53, 69, 77, 79, 87, 95, 101,
    102, 103, 104, 109, 110, 111, 113, 115, 117, 119, 123, 135, 137, 138, 139, 143, 161, 179,
    389, 427, 465, 512, 513, 514, 515, 526, 530, 531, 532, 540, 548, 554, 556, 563, 587, 601,
    636, 989, 990, 993, 995, 1719, 1720, 1723, 2049, 3659, 4045, 5060, 5061, 6000, 6566,
    6665, 6666, 6667, 6668, 6669, 6697, 10080,
]);

const REACHABILITY_PROBE_TIMEOUT_MILLISECONDS = 5_000;

async function hostAnswersWithoutCors(url: string): Promise<boolean> {
    const abort = new AbortController();
    const timeout = setTimeout(() => abort.abort(), REACHABILITY_PROBE_TIMEOUT_MILLISECONDS);

    try {
        await fetch(url, {
            method: "HEAD",
            mode: "no-cors",
            credentials: "omit",
            signal: abort.signal,
        });
        return true;
    } catch {
        return false;
    } finally {
        clearTimeout(timeout);
    }
}

async function describeFetchFailure(
    thrown: unknown,
    host: string,
    port: number,
    timeoutMilliseconds: number,
    attemptedUrl: string,
    proxy: string | undefined,
): Promise<{ statusText: string; detail: string }> {
    const reason = thrown instanceof Error ? thrown.message : String(thrown);

    if (thrown instanceof Error && thrown.name === "AbortError") {
        const seconds = Math.round(timeoutMilliseconds / 1000);
        return { statusText: "Upstream Timeout", detail: `timed out after ${seconds}s` };
    }

    if (!(thrown instanceof TypeError) || !corsIsEnforced()) {
        return { statusText: "Bad Gateway", detail: reason };
    }

    if (attemptedUrl.startsWith("http://") && pageIsServedOverHttps()) {
        return {
            statusText: "Blocked as mixed content",
            detail:
                `${reason}. The page hosting vpod is served over https, so the browser ` +
                `refused this plaintext http request before vpod saw a response. No header ` +
                `${host} could send would allow it. Ask for https://${host} instead, or use ` +
                `Node, which uses real sockets and has no such rule.`,
        };
    }

    if (browserIsOffline()) {
        return {
            statusText: "Browser Offline",
            detail:
                `${reason}. The browser reports no network connection, so this request ` +
                `never left the machine. Nothing about ${host} or its headers is implicated.`,
        };
    }

    if (BROWSER_BLOCKED_PORTS.has(port)) {
        return {
            statusText: "Port Blocked By Browser",
            detail:
                `${reason}. Browsers refuse to fetch port ${port} at all, whatever ${host} ` +
                `sends back, so no header and no proxy makes this reachable. Use a port off ` +
                `that list, or Node, which uses real sockets and has no such rule.`,
        };
    }

    const target = proxy === undefined ? host : `the corsProxy at ${proxy}`;

    // The determination: with CORS out of the way, does anything answer?
    if (await hostAnswersWithoutCors(attemptedUrl)) {
        return {
            statusText: "Blocked by browser CORS policy",
            detail:
                `${reason}. ${target} answered a no-cors probe, so it is reachable and this ` +
                `is its CORS policy: it sends no access-control-allow-origin this page is ` +
                `allowed to use. In a browser every request the guest makes goes out as a ` +
                `fetch, so a host that does not opt in is unreachable. ` +
                (proxy === undefined
                    ? `Pass corsProxy to Sandbox.create with a relay you operate and this ` +
                      `becomes reachable; see infra/cors-proxy/ for one to deploy.`
                    : `Check that ${host} is in the proxy's allowlist and that the proxy ` +
                      `returns access-control-allow-origin for this page.`) +
                ` The same request works under Node, which uses real sockets and does not ` +
                `enforce CORS.`,
        };
    }

    return {
        statusText: "Host Unreachable",
        detail:
            `${reason}. ${target} did not answer a no-cors probe either, so this is not a ` +
            `CORS rejection: the name did not resolve, nothing accepted the connection, or ` +
            `it was dropped in flight. Check the address and that the host is up.`,
    };
}

export class FetchDriver {
    readonly #connections = new Map<number, Connection>();
    readonly #options: DriverOptions;
    readonly #warnedHosts = new Set<string>();

    constructor(options: DriverOptions = {}) {
        this.#options = options;
    }

    #proxied(url: string): string {
        return `${this.#options.corsProxy!.replace(/\/+$/, "")}/${url}`;
    }

    #warnOnce(host: string, message: string): void {
        if (this.#warnedHosts.has(host)) {
            return;
        }
        this.#warnedHosts.add(host);
        console.warn(message);
    }

    handle(command: DriverCommand): void {
        switch (command.kind) {
            case "open":
                this.#connections.set(command.id, {
                    writer: new RingWriter(command.ring),
                    buffered: new Uint8Array(0),
                    host: undefined,
                    resolvedHostname: command.resolvedHostname,
                    port: command.port,
                    secure: false,
                    queue: Promise.resolve(),
                    guestFinishedWriting: false,
                    closed: false,
                });
                return;

            case "send": {
                const connection = this.#connections.get(command.id);
                if (connection === undefined || connection.closed) {
                    return;
                }
                connection.buffered = concat(connection.buffered, new Uint8Array(command.bytes));
                this.#advance(command.id, connection);
                return;
            }

            case "shutdown": {
                const connection = this.#connections.get(command.id);
                if (connection === undefined) {
                    return;
                }
                connection.guestFinishedWriting = true;
                this.#advance(command.id, connection);
                return;
            }

            case "close": {
                const connection = this.#connections.get(command.id);
                if (connection !== undefined) {
                    connection.closed = true;
                }
                this.#connections.delete(command.id);
                return;
            }
        }
    }

    #advance(id: number, connection: Connection): void {
        if (connection.host === undefined) {
            const preamble = parsePreamble(connection.buffered);

            if (preamble.kind === "incomplete") {
                return;
            }

            if (preamble.kind === "ok") {
                connection.host = preamble.preamble.host;
                connection.port = preamble.preamble.port;
                connection.secure = true;
                connection.buffered = connection.buffered.slice(preamble.preamble.consumed);
            } else if (connection.resolvedHostname !== undefined) {
                connection.host = connection.resolvedHostname;
                connection.secure = false;
            } else {
                this.#failConnection(id, connection, preamble.reason);
                return;
            }
        }

        for (;;) {
            const parsed = parseRequest(connection.buffered);

            if (parsed.kind === "incomplete") {
                return;
            }

            if (parsed.kind === "invalid") {
                this.#failConnection(id, connection, parsed.reason);
                return;
            }

            const request = parsed.request;
            connection.buffered = connection.buffered.slice(request.consumed);

            const host = connection.host;
            const port = connection.port;

            const secure = connection.secure;

            connection.queue = connection.queue.then(() =>
                this.#dispatch(connection, request, host, port, secure),
            );

            if (!request.keepAlive) {
                connection.queue = connection.queue.then(() => {
                    connection.writer.end();
                });
                return;
            }
        }
    }

    async #dispatch(
        connection: Connection,
        request: ParsedRequest,
        host: string,
        port: number,
        secure: boolean,
    ): Promise<void> {
        if (connection.closed) {
            return;
        }

        const fetchable = toFetchable(request, host, port, secure);

        const abort = new AbortController();
        const timeoutMilliseconds =
            this.#options.requestTimeoutMilliseconds ?? DEFAULT_REQUEST_TIMEOUT_MILLISECONDS;
        const timeout = setTimeout(() => abort.abort(), timeoutMilliseconds);

        const proxy = this.#options.corsProxy;
        const attemptedUrl = proxy === undefined ? fetchable.url : this.#proxied(fetchable.url);

        try {
            const response = await fetch(attemptedUrl, {
                method: fetchable.method,
                headers: fetchable.headers,
                body: fetchable.body as BodyInit | undefined,
                signal: abort.signal,
                redirect: "manual",
                credentials: "omit",
            });

            const body = new Uint8Array(await response.arrayBuffer());
            const headers: [string, string][] = [];
            response.headers.forEach((value, name) => {
                headers.push([name, value]);
            });

            await this.#writeAll(
                connection,
                serializeResponse(
                    response.status,
                    response.statusText,
                    headers,
                    body,
                    request.keepAlive,
                ),
            );
        } catch (thrown: unknown) {
            const failure = await describeFetchFailure(
                thrown,
                host,
                port,
                timeoutMilliseconds,
                attemptedUrl,
                proxy,
            );

            const viaProxy = proxy === undefined ? "" : ` (via corsProxy at ${proxy})`;
            const message = `fetch to ${fetchable.url} failed: ${failure.detail}${viaProxy}`;
            this.#warnOnce(host, `vpod: ${message}`);
            await this.#writeAll(
                connection,
                serializeTransportError(message, failure.statusText),
            );
            connection.writer.end();
        } finally {
            clearTimeout(timeout);
        }
    }

    async #writeAll(connection: Connection, bytes: Uint8Array): Promise<void> {
        let offset = 0;

        while (offset < bytes.length) {
            const written = connection.writer.write(bytes.subarray(offset));
            offset += written;

            if (offset < bytes.length && !(await connection.writer.waitForSpace())) {
                connection.writer.fail();
                return;
            }
        }
    }

    #failConnection(id: number, connection: Connection, reason: string): void {
        connection.closed = true;
        void this.#writeAll(connection, serializeTransportError(reason)).then(() => {
            connection.writer.end();
        });

        this.#connections.delete(id);
    }
}
