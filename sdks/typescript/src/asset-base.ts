let assetBaseUrl: string | null = null;

/**
 * Where a module's sibling assets live, or null when that cannot be known.
 */
export function directoryOf(moduleUrl: string, relativePath: string): URL | null {
    try {
        return new URL(relativePath, moduleUrl);
    } catch {
        return null;
    }
}

export function setAssetBaseUrl(url: string | URL | null): void {
    assetBaseUrl = url === null ? null : String(url);
}

export function setAssetBaseUrlIfUnset(url: string | URL | null): void {
    if (url !== null) {
        assetBaseUrl ??= String(url);
    }
}

export function assetUrlOrNull(relativePath: string): URL | null {
    return assetBaseUrl === null ? null : new URL(relativePath, assetBaseUrl);
}

export function assetUrl(relativePath: string): URL {
    const url = assetUrlOrNull(relativePath);
    if (url === null) {
        throw new Error(
            `vpod: no asset base is set, so '${relativePath}' cannot be located. ` +
                `Import the package entry point, or pass componentUrl and workerUrl ` +
                `explicitly.`,
        );
    }
    return url;
}
