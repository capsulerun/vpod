export interface SecretSpec {
    /** The credential itself. It never enters the sandbox. */
    value: string;
    /** Hosts this credential may be sent to. `*.example.com` matches one label. */
    hosts: string | string[];
    /** What the guest sees instead. Generated when left out. */
    placeholder?: string;
}

export type SecretsSpec = Record<string, SecretSpec>;

export interface SecretBinding {
    name: string;
    placeholder: string;
    value: string;
    hosts: string[];
}

const PLAIN_IDENTIFIER = /^[A-Za-z_][A-Za-z0-9_]*$/;

function randomSuffix(): string {
    const bytes = new Uint8Array(4);
    crypto.getRandomValues(bytes);

    return Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0")).join("");
}

/**
 * A credential the sandbox can spend but never read.
 *
 * The guest is given a stand-in through its environment. The value itself goes
 * only to the network gateway, which swaps it back in on the way out, and only
 * for a host on the secret's list.
 */
export function secretBindings(secrets: SecretsSpec | undefined): SecretBinding[] {
    if (secrets === undefined) {
        return [];
    }
    if (typeof secrets !== "object" || secrets === null || Array.isArray(secrets)) {
        throw new Error(
            `vpod: secrets must be an object of name to { value, hosts }, got ${JSON.stringify(secrets)}`,
        );
    }

    return Object.entries(secrets).map(([name, spec]) => {
        if (!PLAIN_IDENTIFIER.test(name)) {
            throw new Error(`vpod: secret name ${JSON.stringify(name)} is not a plain identifier`);
        }
        if (typeof spec !== "object" || spec === null) {
            throw new Error(
                `vpod: secret ${JSON.stringify(name)} must be an object with value and hosts`,
            );
        }

        const { value, hosts, placeholder } = spec;
        if (typeof value !== "string" || value.length === 0) {
            throw new Error(`vpod: secret ${JSON.stringify(name)} needs a non-empty string value`);
        }

        const list = typeof hosts === "string" ? [hosts] : hosts;
        if (
            !Array.isArray(list) ||
            list.length === 0 ||
            !list.every((host) => typeof host === "string" && host.length > 0)
        ) {
            throw new Error(
                `vpod: secret ${JSON.stringify(name)} needs at least one host it may be sent to`,
            );
        }

        if (placeholder !== undefined && (typeof placeholder !== "string" || !placeholder)) {
            throw new Error(`vpod: secret ${JSON.stringify(name)} has an empty placeholder`);
        }

        return {
            name,
            placeholder: placeholder ?? `vpod-secret-${name.toLowerCase()}-${randomSuffix()}`,
            value,
            hosts: [...list],
        };
    });
}
