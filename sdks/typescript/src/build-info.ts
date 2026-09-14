/**
 * Facts about the bundled engine, stamped in by `scripts/build.mjs`.
 */

export type BundledTier = "aot" | "base";

interface BuildInfo {
    version: string;
    bundledTier: BundledTier | null;
    engineInterface: string | null;
}

declare const __VPOD_BUILD_INFO__: BuildInfo | undefined;

export const buildInfo: BuildInfo =
    typeof __VPOD_BUILD_INFO__ === "undefined"
        ? { version: "0.0.0", bundledTier: null, engineInterface: null }
        : __VPOD_BUILD_INFO__;

export function bundledEngineInterface(): string | null {
    const override = (globalThis as { process?: { env?: Record<string, string | undefined> } })
        .process?.env?.VPOD_ENGINE_INTERFACE;
    return override !== undefined && override !== "" ? override : buildInfo.engineInterface;
}
