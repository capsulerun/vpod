"""Probe: suspend/resume still works with the sentinel prompt."""
from vpod import Sandbox

with Sandbox.create() as sandbox:
    sandbox.commands.run("export MARK=alive && touch /root/state")
    r = sandbox.commands.run("echo before-$MARK")
    print("before suspend:", r.stdout.strip(), "| success:", r.success)
    instance = sandbox.suspend()

sandbox = Sandbox.resume(instance)
r = sandbox.commands.run("echo resumed-$MARK && ls /root/state")
print("after resume:", r.stdout.strip().replace("\n", " "), "| success:", r.success)

r = sandbox.commands.run("printf 'x # '")
m = [sandbox.commands.run(f"echo m{i}").stdout.strip() for i in range(3)]
print(f"prompt collision on resumed session: stdout={r.stdout!r} markers={m}")
sandbox.close()
Sandbox.destroy(instance)
