def unwrap_result(value):
    """
    Unwrap a WIT result type returned by wasmtime-py.
    """
    if hasattr(value, 'tag'):
        if value.tag == 'err':
            raise RuntimeError(f"WASM call failed: {value.payload}")
        return value.payload

    return value
