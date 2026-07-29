/**
 * Replaces the stock browser `wasi:sockets`.
 */

export type IpAddressFamily = "ipv4" | "ipv6";

export interface IpSocketAddress {
    tag: IpAddressFamily;
    val: unknown;
}

export type ShutdownType = "receive" | "send" | "both";


export interface TcpConnection {
    startConnect(remoteAddress: IpSocketAddress): void;
    finishConnect(): [object, object];
    subscribe(): object;
    localAddress(): IpSocketAddress;
    remoteAddress(): IpSocketAddress;
    shutdown(kind: ShutdownType): void;
    dispose(): void;
}

export interface UdpConnection {
    startConnect(remoteAddress: IpSocketAddress): void;
    finishConnect(): [object, object];
    subscribe(): object;
    localAddress(): IpSocketAddress;
    remoteAddress(): IpSocketAddress;
    dispose(): void;
}

export interface AddressResolution {
    resolveNextAddress(): unknown;
    subscribe(): object;
    dispose(): void;
}

export interface SocketBackend {
    readonly name: string;
    createTcpConnection(family: IpAddressFamily): TcpConnection;
    createUdpConnection(family: IpAddressFamily): UdpConnection;
    resolveAddresses(hostname: string): AddressResolution;
}

const offlineBackend: SocketBackend = {
    name: "offline",
    createTcpConnection(): TcpConnection {
        throw "access-denied";
    },
    createUdpConnection(): UdpConnection {
        throw "access-denied";
    },
    resolveAddresses(): AddressResolution {
        throw "permanent-resolver-failure";
    },
};

let activeBackend: SocketBackend = offlineBackend;

export function setSocketBackend(backend: SocketBackend): void {
    activeBackend = backend;
}

export function socketBackendName(): string {
    return activeBackend.name;
}

class Network {}

class TcpSocket {
    #connection: TcpConnection;

    constructor(connection: TcpConnection) {
        this.#connection = connection;
    }

    startBind(): void {
        throw "access-denied";
    }

    finishBind(): void {
        throw "access-denied";
    }

    startConnect(_network: Network, remoteAddress: IpSocketAddress): void {
        this.#connection.startConnect(remoteAddress);
    }

    finishConnect(): [object, object] {
        return this.#connection.finishConnect();
    }

    startListen(): void {
        throw "access-denied";
    }

    finishListen(): void {
        throw "access-denied";
    }

    accept(): never {
        throw "not-supported";
    }

    localAddress(): IpSocketAddress {
        return this.#connection.localAddress();
    }

    remoteAddress(): IpSocketAddress {
        return this.#connection.remoteAddress();
    }

    isListening(): boolean {
        return false;
    }

    addressFamily(): IpAddressFamily {
        return "ipv4";
    }

    setListenBacklogSize(): void {
        throw "not-supported";
    }

    keepAliveEnabled(): boolean {
        return false;
    }

    setKeepAliveEnabled(): void {}

    keepAliveIdleTime(): bigint {
        return 0n;
    }

    setKeepAliveIdleTime(): void {}

    keepAliveInterval(): bigint {
        return 0n;
    }

    setKeepAliveInterval(): void {}

    keepAliveCount(): number {
        return 0;
    }

    setKeepAliveCount(): void {}

    hopLimit(): number {
        return 64;
    }

    setHopLimit(): void {}

    receiveBufferSize(): bigint {
        return 0n;
    }

    setReceiveBufferSize(): void {}

    sendBufferSize(): bigint {
        return 0n;
    }

    setSendBufferSize(): void {}

    subscribe(): object {
        return this.#connection.subscribe();
    }

    shutdown(kind: ShutdownType): void {
        this.#connection.shutdown(kind);
    }

    [Symbol.dispose](): void {
        this.#connection.dispose();
    }
}

class UdpSocket {
    #connection: UdpConnection;

    constructor(connection: UdpConnection) {
        this.#connection = connection;
    }

    startBind(): void {
        throw "access-denied";
    }

    finishBind(): void {
        throw "access-denied";
    }

    stream(remoteAddress: IpSocketAddress | undefined): [object, object] {
        if (remoteAddress !== undefined) {
            this.#connection.startConnect(remoteAddress);
        }
        return this.#connection.finishConnect();
    }

    localAddress(): IpSocketAddress {
        return this.#connection.localAddress();
    }

    remoteAddress(): IpSocketAddress {
        return this.#connection.remoteAddress();
    }

    addressFamily(): IpAddressFamily {
        return "ipv4";
    }

    unicastHopLimit(): number {
        return 64;
    }

    setUnicastHopLimit(): void {}

    receiveBufferSize(): bigint {
        return 0n;
    }

    setReceiveBufferSize(): void {}

    sendBufferSize(): bigint {
        return 0n;
    }

    setSendBufferSize(): void {}

    subscribe(): object {
        return this.#connection.subscribe();
    }

    [Symbol.dispose](): void {
        this.#connection.dispose();
    }
}

class IncomingDatagramStream {
    receive(): never {
        throw "access-denied";
    }

    subscribe(): never {
        throw "access-denied";
    }
}

class OutgoingDatagramStream {
    checkSend(): bigint {
        return 0n;
    }

    send(): never {
        throw "access-denied";
    }

    subscribe(): never {
        throw "access-denied";
    }
}

class ResolveAddressStream {
    #resolution: AddressResolution;

    constructor(resolution: AddressResolution) {
        this.#resolution = resolution;
    }

    resolveNextAddress(): unknown {
        return this.#resolution.resolveNextAddress();
    }

    subscribe(): object {
        return this.#resolution.subscribe();
    }

    [Symbol.dispose](): void {
        this.#resolution.dispose();
    }
}

export const network = { Network };

export const instanceNetwork = {
    instanceNetwork: (): Network => new Network(),
};

export const ipNameLookup = {
    ResolveAddressStream,
    resolveAddresses: (_network: Network, hostname: string): ResolveAddressStream =>
        new ResolveAddressStream(activeBackend.resolveAddresses(hostname)),
};

export const tcp = { TcpSocket };

export const tcpCreateSocket = {
    createTcpSocket: (family: IpAddressFamily): TcpSocket =>
        new TcpSocket(activeBackend.createTcpConnection(family)),
};

export const udp = { UdpSocket, IncomingDatagramStream, OutgoingDatagramStream };

export const udpCreateSocket = {
    createUdpSocket: (family: IpAddressFamily): UdpSocket =>
        new UdpSocket(activeBackend.createUdpConnection(family)),
};
