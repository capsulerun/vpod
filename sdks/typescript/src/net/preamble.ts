/**
 * `VPOD-CONNECT <host> <port>\n`, the first line on every connection a host transport is handed.
 */

const PREFIX = "VPOD-CONNECT ";
const NEWLINE = 0x0a;

const PREAMBLE_MAX_BYTES = 280;

export interface Preamble {
    host: string;
    port: number;
    consumed: number;
}

export type PreambleResult =
    | { kind: "ok"; preamble: Preamble }
    | { kind: "incomplete" }
    | { kind: "invalid"; reason: string };

export function parsePreamble(bytes: Uint8Array): PreambleResult {
    const end = bytes.indexOf(NEWLINE);

    if (end === -1) {
        if (bytes.length > PREAMBLE_MAX_BYTES) {
            return { kind: "invalid", reason: "no preamble line within the first 280 bytes" };
        }
        return { kind: "incomplete" };
    }

    const line = new TextDecoder().decode(bytes.subarray(0, end)).trimEnd();

    if (!line.startsWith(PREFIX)) {
        return { kind: "invalid", reason: `expected a ${PREFIX.trim()} line, got ${line.slice(0, 40)}` };
    }

    const fields = line.slice(PREFIX.length).split(" ");
    if (fields.length !== 2) {
        return { kind: "invalid", reason: `expected host and port, got ${fields.length} fields` };
    }

    const [host, rawPort] = fields as [string, string];
    const port = Number(rawPort);

    if (host === "") {
        return { kind: "invalid", reason: "empty host" };
    }

    if (!Number.isInteger(port) || port < 1 || port > 65535) {
        return { kind: "invalid", reason: `bad port ${rawPort}` };
    }

    return { kind: "ok", preamble: { host, port, consumed: end + 1 } };
}
