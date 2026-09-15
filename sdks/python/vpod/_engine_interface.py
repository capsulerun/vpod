"""The interface fingerprint of an engine component.

Two engines with the same fingerprint present the same surface to an SDK: the
same WIT types and wiring, the same adapter modules, and a main core module
with the same imports and exports. An SDK can then run either one, whatever
guest code each was translated from.

This file is the reference implementation. The TypeScript build and the
snapshot builder run it rather than reimplementing it, so there is exactly one
definition of "compatible".

Usage: python3 _engine_interface.py <component.wasm>...
"""

import hashlib
import struct
import sys
from pathlib import Path

FINGERPRINT_VERSION = "v1"

_COMPONENT_PREAMBLE_LENGTH = 8
_CUSTOM_SECTION = 0
_CORE_MODULE_SECTION = 1

_CORE_TYPE_SECTION = 1
_CORE_IMPORT_SECTION = 2
_CORE_FUNCTION_SECTION = 3
_CORE_EXPORT_SECTION = 7

_FUNCTION_TYPE_FORM = 0x60
_SIMPLE_VALUE_TYPES = {0x7F, 0x7E, 0x7D, 0x7C, 0x7B, 0x70, 0x6F}

_IMPORT_KIND_FUNCTION = 0
_IMPORT_KIND_TABLE = 1
_IMPORT_KIND_MEMORY = 2
_IMPORT_KIND_GLOBAL = 3
_IMPORT_KIND_TAG = 4


class NotAnEngineComponent(ValueError):
    """The bytes are not a component with the shape an engine has."""


def _read_unsigned(data: bytes, offset: int) -> tuple[int, int]:
    result = 0
    shift = 0
    while True:
        byte = data[offset]
        offset += 1
        result |= (byte & 0x7F) << shift
        if byte < 0x80:
            return result, offset
        shift += 7


def _sections(data: bytes, start: int):
    offset = start
    while offset < len(data):
        section_id = data[offset]
        size, body_start = _read_unsigned(data, offset + 1)
        body_end = body_start + size
        if body_end > len(data):
            raise NotAnEngineComponent("a section runs past the end of the file")
        yield section_id, body_start, body_end
        offset = body_end


def _read_name(data: bytes, offset: int) -> tuple[bytes, int]:
    length, offset = _read_unsigned(data, offset)
    return data[offset:offset + length], offset + length


def _skip_limits(data: bytes, offset: int) -> int:
    has_maximum = data[offset] & 1
    offset += 1
    _, offset = _read_unsigned(data, offset)
    if has_maximum:
        _, offset = _read_unsigned(data, offset)
    return offset


def _length_prefixed(value: bytes) -> bytes:
    return struct.pack("<I", len(value)) + value


def _core_module_surface(module: bytes) -> bytes:
    """Imports and exports of a core module, with every function's signature."""
    if module[:4] != b"\0asm":
        raise NotAnEngineComponent("core module 0 is not a wasm module")

    function_types: list[bytes] = []
    function_type_indices: list[int] = []
    imports = bytearray()
    exports: list[tuple[bytes, int, int]] = []

    for section_id, body_start, body_end in _sections(module, 8):
        if section_id == _CORE_TYPE_SECTION:
            count, offset = _read_unsigned(module, body_start)
            for _ in range(count):
                if module[offset] != _FUNCTION_TYPE_FORM:
                    raise NotAnEngineComponent("core module 0 uses a non-function type")
                signature_start = offset
                offset += 1
                for _ in range(2):
                    value_count, offset = _read_unsigned(module, offset)
                    values = module[offset:offset + value_count]
                    if not set(values) <= _SIMPLE_VALUE_TYPES:
                        raise NotAnEngineComponent("core module 0 uses a compound value type")
                    offset += value_count
                function_types.append(module[signature_start:offset])

        elif section_id == _CORE_IMPORT_SECTION:
            count, offset = _read_unsigned(module, body_start)
            for _ in range(count):
                module_name, offset = _read_name(module, offset)
                field_name, offset = _read_name(module, offset)
                kind = module[offset]
                offset += 1
                imports += _length_prefixed(module_name) + _length_prefixed(field_name)
                imports.append(kind)
                if kind == _IMPORT_KIND_FUNCTION:
                    type_index, offset = _read_unsigned(module, offset)
                    function_type_indices.append(type_index)
                    imports += _length_prefixed(function_types[type_index])
                elif kind == _IMPORT_KIND_TABLE:
                    imports.append(module[offset])
                    offset = _skip_limits(module, offset + 1)
                elif kind == _IMPORT_KIND_MEMORY:
                    offset = _skip_limits(module, offset)
                elif kind == _IMPORT_KIND_GLOBAL:
                    imports += module[offset:offset + 2]
                    offset += 2
                elif kind == _IMPORT_KIND_TAG:
                    _, offset = _read_unsigned(module, offset + 1)
                else:
                    raise NotAnEngineComponent(f"core module 0 has an import of kind {kind}")

        elif section_id == _CORE_FUNCTION_SECTION:
            count, offset = _read_unsigned(module, body_start)
            for _ in range(count):
                type_index, offset = _read_unsigned(module, offset)
                function_type_indices.append(type_index)

        elif section_id == _CORE_EXPORT_SECTION:
            count, offset = _read_unsigned(module, body_start)
            for _ in range(count):
                field_name, offset = _read_name(module, offset)
                kind = module[offset]
                index, offset = _read_unsigned(module, offset + 1)
                exports.append((field_name, kind, index))

    described_exports = bytearray()
    for field_name, kind, index in exports:
        described_exports += _length_prefixed(field_name)
        described_exports.append(kind)
        if kind == _IMPORT_KIND_FUNCTION:
            described_exports += _length_prefixed(function_types[function_type_indices[index]])

    return _length_prefixed(bytes(imports)) + _length_prefixed(bytes(described_exports))


def fingerprint(component: bytes) -> str:
    """The interface fingerprint of a component, as `v1:<sha256 hex>`."""
    if component[:4] != b"\0asm" or len(component) < _COMPONENT_PREAMBLE_LENGTH:
        raise NotAnEngineComponent("not a wasm binary")
    try:
        return _fingerprint(component)
    except IndexError as truncated:
        raise NotAnEngineComponent("the component ends in the middle of a section") from truncated


def _fingerprint(component: bytes) -> str:

    digest = hashlib.sha256(f"vpod-engine-interface-{FINGERPRINT_VERSION}\0".encode())
    core_modules: list[bytes] = []

    for section_id, body_start, body_end in _sections(component, _COMPONENT_PREAMBLE_LENGTH):
        if section_id == _CUSTOM_SECTION:
            continue
        body = component[body_start:body_end]
        if section_id == _CORE_MODULE_SECTION:
            core_modules.append(body)
            continue
        digest.update(bytes([section_id]) + _length_prefixed(body))

    if not core_modules:
        raise NotAnEngineComponent("the component embeds no core module")

    for adapter in core_modules[1:]:
        digest.update(b"adapter" + _length_prefixed(adapter))
    digest.update(b"main" + _core_module_surface(core_modules[0]))

    return f"{FINGERPRINT_VERSION}:{digest.hexdigest()}"


if __name__ == "__main__":
    if len(sys.argv) < 2:
        print(__doc__.strip().splitlines()[-1], file=sys.stderr)
        sys.exit(2)
    for path in sys.argv[1:]:
        result = fingerprint(Path(path).read_bytes())
        print(result if len(sys.argv) == 2 else f"{result}  {path}")
