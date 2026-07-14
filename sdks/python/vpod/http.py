from dataclasses import dataclass, field

from ._result import unwrap_result


@dataclass
class HttpResponse:
    status: int
    headers: dict[str, str] = field(default_factory=dict)
    body: bytes = b""

    @property
    def ok(self) -> bool:
        return 200 <= self.status < 300

    @property
    def text(self) -> str:
        return self.body.decode("utf-8", errors="replace")


def _make_header(name: str, value: str):
    header = object.__new__(type("HttpHeader", (), {}))
    object.__setattr__(header, "name", name)
    object.__setattr__(header, "value", value)
    return header


class Http:

    def __init__(self, exports):
        self._exports = exports

    def request(
        self,
        method: str,
        url: str,
        headers: dict[str, str] | None = None,
        body: bytes | str | None = None,
    ) -> HttpResponse:
        header_list = [_make_header(k, v) for k, v in (headers or {}).items()]
        if isinstance(body, str):
            body = body.encode("utf-8")

        result = unwrap_result(
            self._exports["http-fetch"](method.upper(), url, header_list, body)
        )

        return HttpResponse(
            status=result.status,
            headers={h.name: h.value for h in result.headers},
            body=bytes(result.body),
        )

    def get(self, url: str, headers: dict[str, str] | None = None) -> HttpResponse:
        return self.request("GET", url, headers=headers)

    def post(
        self,
        url: str,
        body: bytes | str | None = None,
        headers: dict[str, str] | None = None,
    ) -> HttpResponse:
        return self.request("POST", url, headers=headers, body=body)
