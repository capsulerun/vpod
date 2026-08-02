const { sockets } = await import("@bytecodealliance/preview2-shim");
const net = sockets.instanceNetwork.instanceNetwork();

for (const name of ["localhost"]) {
  const s = sockets.ipNameLookup.resolveAddresses(net, name);
  s.subscribe().block();

  const out = [];

  for (;;) {
    const a = s.resolveNextAddress();
    if (a === undefined) break;
    out.push(a);
  }

  console.log(name, "->", JSON.stringify(out));
}
