let assetBaseUrl: string | null = null;

export function directoryOf(moduleUrl: string, relativePath: string): URL {
    const base: string = moduleUrl;
    return new URL(relativePath, base);
}

export function setAssetBaseUrl(url: string | URL): void {
    assetBaseUrl = String(url);
}

export function setAssetBaseUrlIfUnset(url: string | URL): void {
    assetBaseUrl ??= String(url);
}

export function assetUrl(relativePath: string): URL {
    if (assetBaseUrl === null) {
        throw new Error(
            `vpod: no asset base is set, so '${relativePath}' cannot be located. ` +
                `Import the package entry point, or pass componentUrl and workerUrl ` +
                `explicitly.`,
        );
    }
    return new URL(relativePath, assetBaseUrl);
}
