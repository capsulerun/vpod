import { execFileSync } from "node:child_process";
import { existsSync } from "node:fs";

const MACOS_BINARIES = {
    chrome: "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
    firefox: "/Applications/Firefox.app/Contents/MacOS/firefox",
};

const LINUX_COMMANDS = {
    chrome: ["google-chrome", "google-chrome-stable", "chromium", "chromium-browser"],
    firefox: ["firefox"],
};

function onPath(command) {
    try {
        return execFileSync("which", [command], { encoding: "utf8" }).trim() || null;
    } catch {
        return null;
    }
}

export function locateBrowser(name) {
    if (process.platform === "darwin" && existsSync(MACOS_BINARIES[name] ?? "")) {
        return MACOS_BINARIES[name];
    }
    for (const command of LINUX_COMMANDS[name] ?? []) {
        const found = onPath(command);
        if (found !== null) {
            return found;
        }
    }
    return null;
}

export function browserArguments(name, url, profile) {
    if (name === "firefox") {
        return ["--headless", "--profile", profile, url];
    }

    return [
        "--headless=new",
        "--disable-gpu",
        "--no-first-run",
        "--no-default-browser-check",
        ...(process.platform === "linux" ? ["--no-sandbox", "--disable-dev-shm-usage"] : []),
        `--user-data-dir=${profile}`,
        url,
    ];
}
