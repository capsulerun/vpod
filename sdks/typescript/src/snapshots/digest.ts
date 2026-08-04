/**
 * SHA-256 as lowercase hex, matching the `sha256` field in the registry and `_file_sha256` in the Python SDK.
 */
export async function sha256Hex(bytes: Uint8Array): Promise<string> {
    if (globalThis.crypto?.subtle === undefined) {
        throw new Error(
            "vpod: crypto.subtle is unavailable, which means this page is not a " +
                "secure context. Serve over https, or over http from localhost.",
        );
    }

    const digest = await crypto.subtle.digest("SHA-256", bytes as BufferSource);
    return [...new Uint8Array(digest)]
        .map((byte) => byte.toString(16).padStart(2, "0"))
        .join("");
}
