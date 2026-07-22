import time
t = time.time()
s=0
for i in range(1000000): s=(s+i*i)^(i&0xff)
print(time.time()-t)
