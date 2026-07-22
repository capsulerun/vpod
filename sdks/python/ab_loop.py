import time
from vpod import Sandbox

# NOTE: measure HOST wall-clock around the call. The guest's own time.time()
# reads the emulated clock (deterministic per retired instruction), so it is
# identical for aot and interpreter — only host wall-clock reflects how fast
# we emulate.
LOOP = (
    "s=0\n"
    "for i in range(1000000): s=(s+i*i)^(i&0xff)\n"
    "print(s)\n"
)

with Sandbox.create() as sb:
    sb.code.run("print('warm')")  # pay one-time sandbox/pyrunner warmup
    for n in range(3):
        t = time.time()
        sb.code.run(LOOP)
        print(f"run {n}: host={time.time()-t:.3f}s")
