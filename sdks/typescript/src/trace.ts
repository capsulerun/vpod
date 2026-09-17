export interface TraceSources {
    processes?: boolean;
    files?: boolean;
    network?: boolean;
    mounts?: boolean;
    bufferBytes?: number;
}

export type TraceSetting = boolean | TraceSources;

export interface WireTraceOptions {
    processes: boolean;
    files: boolean;
    network: boolean;
    mounts: boolean;
    bufferBytes: number;
}

export interface TraceEvent {
    v: number;
    seq: number;
    guest_ns: number;
    wall_ms: number;
    kind: string;
    [field: string]: unknown;
}

export interface FileActivity {
    path: string;
    read: boolean;
    written: boolean;
    created: boolean;
    deleted: boolean;
    renamedTo: string | null;
    renamedFrom: string | null;
    denied: boolean;
}

export interface HttpRequest {
    method: string;
    url: string;
}

export interface NetworkActivity {
    host: string | null;
    address: string;
    port: number;
    protocol: string | null;
    requests: HttpRequest[];
    bytesOut: number;
    bytesIn: number;
    failed: boolean;
}

export interface ProcessNode {
    pid: number | null;
    path: string | null;
    argv: string[];
    exitCode: number | null;
    startedAt: number;
    children: ProcessNode[];
}

export const TRACE_NOT_ENABLED =
    "vpod: tracing is not enabled for this sandbox. Create it with " +
    "Sandbox.create({ trace: true }) to record what it does.";

export const TRACE_NOT_SUPPORTED =
    "vpod: this sandbox runs on an engine without trace support, a snapshot's own " +
    'engine from an older vpod release. Create the sandbox with engine: "default" ' +
    "to trace it.";

const SOURCES = ["processes", "files", "network", "mounts"] as const;
const DRAIN_ALL_BYTES = 0xffff_ffff;

const EACCES = -13;
const EPERM = -1;

const NOISE_PREFIXES = ["/proc/", "/sys/", "/dev/", "/etc/ld-musl-"];
const NOISE_DIRECTORIES = ["/proc", "/sys", "/dev"];

export function traceOptions(setting: TraceSetting | undefined): WireTraceOptions | null {
    if (setting === undefined || setting === false) {
        return null;
    }
    if (setting === true) {
        return { processes: true, files: true, network: true, mounts: true, bufferBytes: 0 };
    }
    if (typeof setting !== "object" || setting === null) {
        throw new Error(`vpod: trace must be true or an object of sources, got ${JSON.stringify(setting)}`);
    }

    const known = new Set<string>([...SOURCES, "bufferBytes"]);
    const unknown = Object.keys(setting).filter((key) => !known.has(key));
    if (unknown.length > 0) {
        throw new Error(
            `vpod: unknown trace options ${JSON.stringify(unknown)}, expected ${JSON.stringify([...known])}`,
        );
    }

    return {
        processes: setting.processes === true,
        files: setting.files === true,
        network: setting.network === true,
        mounts: setting.mounts === true,
        bufferBytes: setting.bufferBytes ?? 0,
    };
}

const text = (value: unknown): string | null => (typeof value === "string" ? value : null);
const count = (value: unknown): number => (typeof value === "number" ? value : 0);

function hostOf(url: string): string | null {
    try {
        return new URL(url).hostname.replace(/^\[(.*)\]$/, "$1") || null;
    } catch {
        return null;
    }
}

function isNoise(entry: FileActivity): boolean {
    const path = entry.path;
    if (NOISE_DIRECTORIES.includes(path) || NOISE_PREFIXES.some((prefix) => path.startsWith(prefix))) {
        return true;
    }
    const onlyRead = !(
        entry.written ||
        entry.created ||
        entry.deleted ||
        entry.renamedTo !== null ||
        entry.renamedFrom !== null ||
        entry.denied
    );
    return onlyRead && (path.endsWith(".so") || path.includes(".so."));
}

export class Trace {
    readonly #events: readonly TraceEvent[];

    constructor(events: readonly TraceEvent[]) {
        this.#events = Object.freeze([...events]);
    }

    get events(): readonly TraceEvent[] {
        return this.#events;
    }

    get complete(): boolean {
        return !this.#events.some((event) => event.kind === "trace.dropped");
    }

    toJSONL(): string {
        return this.#events.map((event) => `${JSON.stringify(event)}\n`).join("");
    }

    files(options: { internal?: boolean; noise?: boolean } = {}): FileActivity[] {
        const activities = new Map<string, FileActivity>();

        const activity = (path: unknown): FileActivity | null => {
            if (typeof path !== "string") return null;
            let entry = activities.get(path);
            if (entry === undefined) {
                entry = {
                    path,
                    read: false,
                    written: false,
                    created: false,
                    deleted: false,
                    renamedTo: null,
                    renamedFrom: null,
                    denied: false,
                };
                activities.set(path, entry);
            }
            return entry;
        };

        const markDenied = (path: unknown, result: number) => {
            if (result !== EACCES && result !== EPERM) return;
            const entry = activity(path);
            if (entry !== null) entry.denied = true;
        };

        for (const event of this.#events) {
            if (event.internal === true && !options.internal) continue;
            const result = event.result as number;

            switch (event.kind) {
                case "file.open":
                case "mount.open": {
                    const succeeded = event.kind === "file.open" ? result >= 0 : result === 0;
                    if (!succeeded) {
                        markDenied(event.path, result);
                        break;
                    }
                    const entry = activity(event.path);
                    if (entry === null) break;
                    entry.read ||= event.access === "read" || event.access === "read-write";
                    entry.written ||=
                        event.access === "write" || event.access === "read-write" || event.truncate === true;
                    break;
                }
                case "file.rename":
                case "mount.rename": {
                    if (result !== 0) {
                        markDenied(event.from, result);
                        markDenied(event.to, result);
                        break;
                    }
                    const source = activity(event.from);
                    const destination = activity(event.to);
                    if (source !== null) source.renamedTo = text(event.to);
                    if (destination !== null) {
                        destination.renamedFrom = text(event.from);
                        destination.written = true;
                    }
                    break;
                }
                case "file.delete":
                case "mount.delete": {
                    if (result !== 0) {
                        markDenied(event.path, result);
                        break;
                    }
                    const entry = activity(event.path);
                    if (entry !== null) entry.deleted = true;
                    break;
                }
                case "dir.create":
                case "mount.mkdir":
                case "mount.create": {
                    if (result !== 0) {
                        markDenied(event.path, result);
                        break;
                    }
                    const entry = activity(event.path);
                    if (entry !== null) {
                        entry.created = true;
                        entry.written ||= event.kind === "mount.create";
                    }
                    break;
                }
                case "file.truncate":
                case "mount.truncate": {
                    if (result !== 0) {
                        markDenied(event.path, result);
                        break;
                    }
                    const entry = activity(event.path);
                    if (entry !== null) entry.written = true;
                    break;
                }
                case "mount.close": {
                    const bytesRead = count(event.bytes_read);
                    const bytesWritten = count(event.bytes_written);
                    if (bytesRead === 0 && bytesWritten === 0) break;
                    const entry = activity(event.path);
                    if (entry === null) break;
                    entry.read ||= bytesRead > 0;
                    entry.written ||= bytesWritten > 0;
                    break;
                }
            }
        }

        return [...activities.values()].filter((entry) => options.noise || !isNoise(entry));
    }

    network(options: { internal?: boolean } = {}): NetworkActivity[] {
        const activities = new Map<string, NetworkActivity>();

        for (const event of this.#events) {
            if (event.internal === true && !options.internal) continue;
            if (!["net.connect", "net.flow", "net.udp", "net.http"].includes(event.kind)) continue;
            if (typeof event.address !== "string" || typeof event.port !== "number") continue;

            const key = `${event.address} ${event.port}`;
            let entry = activities.get(key);
            if (entry === undefined) {
                entry = {
                    host: null,
                    address: event.address,
                    port: event.port,
                    protocol: null,
                    requests: [],
                    bytesOut: 0,
                    bytesIn: 0,
                    failed: true,
                };
                activities.set(key, entry);
            }
            entry.host ??= text(event.host);

            switch (event.kind) {
                case "net.connect":
                    entry.protocol ??= text(event.protocol);
                    entry.failed &&= event.result !== 0;
                    break;
                case "net.flow":
                    entry.protocol ??= text(event.protocol);
                    entry.bytesOut += count(event.bytes_out);
                    entry.bytesIn += count(event.bytes_in);
                    entry.failed &&= event.failed === true;
                    break;
                case "net.udp":
                    entry.protocol ??= "udp";
                    entry.failed = false;
                    break;
                case "net.http":
                    entry.host ??= hostOf(String(event.url));
                    entry.requests.push({ method: String(event.method), url: String(event.url) });
                    break;
            }
        }

        return [...activities.values()];
    }

    processes(options: { internal?: boolean } = {}): ProcessNode[] {
        const nodes: { node: ProcessNode; internal: boolean }[] = [];
        const runningByTask = new Map<string, ProcessNode>();

        for (const event of this.#events) {
            if (event.kind === "process.exec" && !("result" in event)) {
                const node: ProcessNode = {
                    pid: null,
                    path: text(event.path),
                    argv: Array.isArray(event.argv) ? event.argv.map(String) : [],
                    exitCode: null,
                    startedAt: event.guest_ns,
                    children: [],
                };
                nodes.push({ node, internal: event.internal === true });
                runningByTask.set(String(event.task), node);
            } else if (event.kind === "process.exit") {
                const task = String(event.task);
                const node = runningByTask.get(task);
                if (node !== undefined) {
                    node.exitCode = event.code as number;
                    runningByTask.delete(task);
                }
            }
        }

        return nodes.filter((entry) => options.internal || !entry.internal).map((entry) => entry.node);
    }
}

const WATCH_CLOSED = Symbol("watch closed");

class Watcher {
    #queue: (TraceEvent | typeof WATCH_CLOSED)[] = [];
    #wake: (() => void) | null = null;

    push(item: TraceEvent | typeof WATCH_CLOSED): void {
        this.#queue.push(item);
        this.#wake?.();
        this.#wake = null;
    }

    async next(): Promise<TraceEvent | typeof WATCH_CLOSED> {
        while (this.#queue.length === 0) {
            await new Promise<void>((resolve) => {
                this.#wake = resolve;
            });
        }
        return this.#queue.shift()!;
    }
}

export class TraceRecorder {
    readonly #options: WireTraceOptions | null;
    readonly #drainSession: (maxBytes: number) => Promise<Uint8Array | null>;
    readonly #decoder = new TextDecoder();
    #events: TraceEvent[] = [];
    #watchers = new Set<Watcher>();
    #closed = false;

    /** @internal */
    constructor(
        options: WireTraceOptions | null,
        drainSession: (maxBytes: number) => Promise<Uint8Array | null>,
    ) {
        this.#options = options;
        this.#drainSession = drainSession;
    }

    get enabled(): boolean {
        return this.#options !== null;
    }

    async collect(): Promise<Trace> {
        this.#requireEnabled();
        await this._drain();
        return new Trace(this.#events);
    }

    watch(): AsyncIterable<TraceEvent> {
        this.#requireEnabled();
        const watcher = new Watcher();
        if (this.#closed) {
            watcher.push(WATCH_CLOSED);
        } else {
            this.#watchers.add(watcher);
        }
        const watchers = this.#watchers;

        return {
            async *[Symbol.asyncIterator]() {
                try {
                    for (;;) {
                        const next = await watcher.next();
                        if (next === WATCH_CLOSED) return;
                        yield next;
                    }
                } finally {
                    watchers.delete(watcher);
                }
            },
        };
    }

    async clear(): Promise<void> {
        this.#requireEnabled();
        await this._drain();
        this.#events = [];
    }

    #requireEnabled(): void {
        if (!this.enabled) {
            throw new Error(TRACE_NOT_ENABLED);
        }
    }

    /** @internal */
    get _options(): WireTraceOptions | null {
        return this.#options;
    }

    /** @internal */
    async _drain(): Promise<void> {
        if (!this.enabled) return;
        const drained = await this.#drainSession(DRAIN_ALL_BYTES);
        if (drained === null || drained.byteLength === 0) return;

        const events = this.#decoder
            .decode(drained)
            .split("\n")
            .filter((line) => line.length > 0)
            .map((line) => JSON.parse(line) as TraceEvent);

        this.#events.push(...events);
        for (const watcher of this.#watchers) {
            for (const event of events) watcher.push(event);
        }
    }

    /** @internal */
    async _mark(): Promise<number> {
        if (!this.enabled) return 0;
        await this._drain();
        return this.#events.length;
    }

    /** @internal */
    async _since(mark: number): Promise<Trace | null> {
        if (!this.enabled) return null;
        await this._drain();
        return new Trace(this.#events.slice(mark));
    }

    /** @internal */
    _close(): void {
        this.#closed = true;
        for (const watcher of this.#watchers) watcher.push(WATCH_CLOSED);
        this.#watchers.clear();
    }
}
