import { parentPort, workerData } from "node:worker_threads";

const { FetchDriver } = await import(workerData.driverModule);

const makeFetch = new Function(`return (${workerData.script});`)();
globalThis.fetch = makeFetch();

const driver = new FetchDriver(workerData.options ?? {});

parentPort.on("message", (command) => {
    if (command.kind === "shutdown") {
        parentPort.close();
        return;
    }
    driver.handle(command);
});

parentPort.postMessage({ kind: "ready" });
