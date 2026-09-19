export type EnvSpec = Record<string, string>;

export interface EnvVar {
    name: string;
    value: string;
}

const PLAIN_IDENTIFIER = /^[A-Za-z_][A-Za-z0-9_]*$/;

/**
 * The guest receives these as a shell `export`, so a name that is not a plain
 * identifier could carry arbitrary shell along with it.
 */
export function envVars(env: EnvSpec | undefined): EnvVar[] {
    if (env === undefined) {
        return [];
    }
    if (typeof env !== "object" || env === null || Array.isArray(env)) {
        throw new Error(
            `vpod: env must be an object of name to value, got ${JSON.stringify(env)}`,
        );
    }

    return Object.entries(env).map(([name, value]) => {
        if (!PLAIN_IDENTIFIER.test(name)) {
            throw new Error(
                `vpod: environment variable name ${JSON.stringify(name)} is not a plain identifier`,
            );
        }
        if (typeof value !== "string") {
            throw new Error(
                `vpod: environment variable ${JSON.stringify(name)} needs a string value`,
            );
        }

        return { name, value };
    });
}
