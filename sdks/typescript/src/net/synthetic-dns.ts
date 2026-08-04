/**
 * Addresses for a guest
 */

const FIRST_OCTET = 198;
const SECOND_OCTET_BASE = 18;
const HOST_COUNT = 2 * 256 * 256;

export class SyntheticAddresses {
    #byHostname = new Map<string, Uint8Array>();
    #byAddress = new Map<string, string>();
    #next = 1;

    addressFor(hostname: string): Uint8Array {
        const known = this.#byHostname.get(hostname);
        if (known !== undefined) {
            return known;
        }

        if (this.#next >= HOST_COUNT) {
            throw new Error(`synthetic address space exhausted after ${HOST_COUNT} hostnames`);
        }

        const ordinal = this.#next++;
        const address = new Uint8Array([
            FIRST_OCTET,
            SECOND_OCTET_BASE + ((ordinal >> 16) & 0x01),
            (ordinal >> 8) & 0xff,
            ordinal & 0xff,
        ]);

        this.#byHostname.set(hostname, address);
        this.#byAddress.set(address.join("."), hostname);

        return address;
    }

    hostnameFor(address: Uint8Array | number[]): string | undefined {
        return this.#byAddress.get(Array.from(address).join("."));
    }

    get size(): number {
        return this.#byHostname.size;
    }
}
