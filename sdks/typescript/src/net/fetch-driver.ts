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

export class FetchDriver {
    readonly #connections = new Map<number, Connection>();
    readonly #options: DriverOptions;

    constructor(options: DriverOptions = {}) {
        this.#options = options;
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
        const timeout = setTimeout(
            () => abort.abort(),
            this.#options.requestTimeoutMilliseconds ?? DEFAULT_REQUEST_TIMEOUT_MILLISECONDS,
        );

        try {
            const response = await fetch(fetchable.url, {
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
            const reason = thrown instanceof Error ? thrown.message : String(thrown);
            await this.#writeAll(
                connection,
                serializeTransportError(`fetch to ${fetchable.url} failed: ${reason}`),
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
