/**
 * Support for auth snapshots registry
 */

export const PUBLIC_REGISTRY_URL = "https://registry.vpod.sh/v1/snapshots.json";
export const PRIVATE_REGISTRY_URL = "https://api.vpod.sh/v1/snapshots.json";

function fromEnvironment(name: string): string | undefined {
    const environment = (
        globalThis as { process?: { env?: Record<string, string | undefined> } }
    ).process?.env;
    const configured = environment?.[name];
    return configured === undefined || configured === "" ? undefined : configured;
}

export function resolveApiKey(explicit: string | undefined): string | undefined {
    if (explicit !== undefined && explicit !== "") {
        return explicit;
    }
    return fromEnvironment("VPOD_API_KEY");
}


export function isBrowser(): boolean {
    return (
        typeof globalThis === "object" &&
        typeof (globalThis as { document?: unknown }).document === "object" &&
        (globalThis as { document?: unknown }).document !== null
    );
}

export function checkApiKeyKind(apiKey: string): void {
    const browser = isBrowser();

    if (apiKey.startsWith("vpod_sk_") && browser) {
        throw new Error(
            "vpod: this is a secret key (vpod_sk_) and it is being used in a " +
                "browser, where anyone who opens devtools can read it. Use a " +
                "publishable key (vpod_pk_), which is restricted to an " +
                "allowlist of origins.",
        );
    }

    if (apiKey.startsWith("vpod_pk_") && !browser) {
        throw new Error(
            "vpod: this is a publishable key (vpod_pk_) and there is no browser " +
                "here. Publishable keys are protected by an allowlist of Origins, " +
                "and nothing outside a browser sends an Origin the server can " +
                "trust, so the key buys you nothing. Use a secret key (vpod_sk_).",
        );
    }

    if (!apiKey.startsWith("vpod_sk_") && !apiKey.startsWith("vpod_pk_")) {
        throw new Error(
            `vpod: an API key must start with vpod_sk_ (server side) or ` +
                `vpod_pk_ (browser). Got a key starting with ` +
                `${JSON.stringify(apiKey.slice(0, 8))}.`,
        );
    }
}

/** explicit -> VPOD_REGISTRY -> (key ? private : public). One chain, one place. */
export function resolveRegistryUrl(
    explicit: string | undefined,
    apiKey?: string | undefined,
): string {
    if (explicit !== undefined && explicit !== "") {
        return explicit;
    }
    const configured = fromEnvironment("VPOD_REGISTRY");
    if (configured !== undefined) {
        return configured;
    }
    return apiKey === undefined ? PUBLIC_REGISTRY_URL : PRIVATE_REGISTRY_URL;
}

export function sameOrigin(url: string, other: string): boolean {
    try {
        return new URL(url).origin === new URL(other).origin;
    } catch {
        return false;
    }
}

export function authHeaders(
    url: string,
    registryUrl: string,
    apiKey: string | undefined,
): Record<string, string> {
    if (apiKey === undefined || !sameOrigin(url, registryUrl)) {
        return {};
    }
    return { Authorization: `Bearer ${apiKey}` };
}

export async function keyFingerprint(apiKey: string): Promise<string> {
    const digest = await crypto.subtle.digest(
        "SHA-256",
        new TextEncoder().encode(apiKey) as BufferSource,
    );
    return [...new Uint8Array(digest)]
        .map((byte) => byte.toString(16).padStart(2, "0"))
        .join("")
        .slice(0, 12);
}
