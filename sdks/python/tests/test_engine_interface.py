import pytest

from vpod._engine_interface import NotAnEngineComponent, fingerprint

I32 = 0x7F
I64 = 0x7E


def unsigned(value: int) -> bytes:
    encoded = bytearray()
    while True:
        byte = value & 0x7F
        value >>= 7
        if value:
            encoded.append(byte | 0x80)
        else:
            encoded.append(byte)
            return bytes(encoded)


def section(section_id: int, body: bytes) -> bytes:
    return bytes([section_id]) + unsigned(len(body)) + body


def name(text: str) -> bytes:
    return unsigned(len(text)) + text.encode()


def vector(items: list[bytes]) -> bytes:
    return unsigned(len(items)) + b"".join(items)


def function_type(params: list[int], results: list[int]) -> bytes:
    return b"\x60" + vector([bytes([p]) for p in params]) + vector([bytes([r]) for r in results])


def core_module(
    export_params: list[int] = (I32,),
    code_constant: int = 1,
    export_name: str = "session-start",
) -> bytes:
    """A module importing one function and exporting one, with a body we can vary."""
    types = section(1, vector([function_type([I32], []), function_type(list(export_params), [I32])]))
    imports = section(2, vector([name("wasi:io/streams@0.2.9") + name("drop") + b"\x00" + unsigned(0)]))
    functions = section(3, vector([unsigned(1)]))
    exports = section(7, vector([name(export_name) + b"\x00" + unsigned(1)]))
    body = b"\x00" + b"\x41" + unsigned(code_constant) + b"\x0b"
    code = section(10, vector([unsigned(len(body)) + body]))
    return b"\0asm\x01\x00\x00\x00" + types + imports + functions + exports + code


def component(
    main: bytes,
    adapter: bytes = b"\0asm\x01\x00\x00\x00",
    wiring: bytes = b"\x01\x02\x03",
    custom: bytes = b"",
) -> bytes:
    sections = section(1, main) + section(1, adapter) + section(11, wiring)
    if custom:
        sections = section(0, name("producers") + custom) + sections
    return b"\0asm\x0d\x00\x01\x00" + sections


class TestFingerprint:
    def test_is_versioned(self):
        assert fingerprint(component(core_module())).startswith("v1:")

    def test_ignores_guest_code_so_translations_of_one_source_match(self):
        assert fingerprint(component(core_module(code_constant=1))) == fingerprint(
            component(core_module(code_constant=99))
        )

    def test_ignores_custom_sections(self):
        assert fingerprint(component(core_module(), custom=b"rustc 1.98.1")) == fingerprint(
            component(core_module(), custom=b"rustc 1.96.0")
        )

    def test_changes_when_an_export_signature_changes(self):
        assert fingerprint(component(core_module(export_params=[I32]))) != fingerprint(
            component(core_module(export_params=[I64]))
        )

    def test_changes_when_an_export_is_renamed(self):
        assert fingerprint(component(core_module(export_name="session-start"))) != fingerprint(
            component(core_module(export_name="session-begin"))
        )

    def test_changes_when_an_adapter_changes(self):
        assert fingerprint(component(core_module(), adapter=b"\0asm\x01\x00\x00\x00")) != fingerprint(
            component(core_module(), adapter=b"\0asm\x01\x00\x00\x00" + section(0, name("x")))
        )

    def test_changes_when_the_wiring_changes(self):
        assert fingerprint(component(core_module(), wiring=b"\x01")) != fingerprint(
            component(core_module(), wiring=b"\x02")
        )

    def test_refuses_bytes_that_are_not_wasm(self):
        with pytest.raises(NotAnEngineComponent):
            fingerprint(b"VPOD snapshot, not an engine")

    def test_refuses_a_component_without_a_core_module(self):
        with pytest.raises(NotAnEngineComponent):
            fingerprint(b"\0asm\x0d\x00\x01\x00" + section(11, b"\x01"))

    def test_refuses_a_truncated_file(self):
        whole = component(core_module())
        with pytest.raises(NotAnEngineComponent):
            fingerprint(whole[: len(whole) - 2])
