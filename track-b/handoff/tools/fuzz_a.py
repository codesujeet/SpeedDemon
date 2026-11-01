import random, subprocess, os, sys, datetime
S='/tmp/claude-0/-home-user-SpeedDemon/41f9d852-ae71-53d3-97e9-45f8193ee723/scratchpad/'
ALPHA='abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789/-_.{}:'
T0=datetime.datetime(2026,10,30,18)
def ep(rng,pool):
    if pool and rng.random()<0.7: return rng.choice(pool)
    k=rng.random()
    n=1 if k<0.05 else 125 if k<0.1 else rng.randint(2,125) if k<0.3 else rng.randint(2,40)
    s='/'+''.join(rng.choice(ALPHA) for _ in range(n-1))
    if rng.random()<0.3: s=rng.choice(['/v2/orders','/v2/orders-export','/v2/orders.csv','/v2/orders/{id}','/v2/ordersByDate','/v2/orders_legacy','/v2/orderstatus','/v2/rank100','/v2/rank20','/','/health'])
    return s
def lat(rng):
    k=rng.random()
    if k<0.05: return 0
    if k<0.1: return 4294967295
    if k<0.2: return rng.randint(1<<19,4294967294)
    if k<0.25: return (1<<19)-1
    return int(rng.lognormvariate(8.5,1.4))
def line(rng,pool):
    k=rng.random()
    if k<0.05: sec=0
    elif k<0.1: sec=72*3600-1e-6
    elif k<0.2: sec=rng.randint(0,71)*3600+rng.choice([0,3599.999999])
    else: sec=rng.random()*72*3600
    us=int(round(sec*1e6)); d=T0+datetime.timedelta(microseconds=us)
    st=rng.choice([100,199,200,204,301,404,499,500,503,599,rng.randint(100,599)])
    return '%s GET %s %d %d\n'%(d.strftime('%Y-%m-%dT%H:%M:%S.')+'%06dZ'%d.microsecond,ep(rng,pool),st,lat(rng))
bins=sys.argv[2:]; N=int(sys.argv[1]); rng=random.Random(777); bad={b:0 for b in bins}
os.makedirs(S+'edge/fuzza',exist_ok=True)
for case in range(N):
    pool=[ep(rng,None) for _ in range(rng.choice([1,3,20,300]))]
    n=rng.choice([1,2,10,100,2000,6000])
    log=''.join(line(rng,pool) for _ in range(n))
    if rng.random()<0.3: lines=log.split('\n')[:-1]; log='\n'.join(lines[:len(lines)//2]+lines[:5]+lines[len(lines)//2:])+'\n'   # duplicate lines
    open(S+'edge/fuzza/requests.log','w').write(log)
    subprocess.run(['python3',S+'tools/ref_a.py',S+'edge/fuzza',S+'edge/fuzza/want.txt'],check=True)
    want=open(S+'edge/fuzza/want.txt','rb').read()
    for b in bins:
        try: os.unlink('/out/summary.txt')
        except FileNotFoundError: pass
        r=subprocess.run([S+'g15/'+b],capture_output=True)
        got=open('/out/summary.txt','rb').read() if os.path.exists('/out/summary.txt') else b'<none>'
        if r.returncode!=0 or got!=want:
            bad[b]+=1
            if bad[b]<=2: print('MISMATCH',b,'case',case,'rc',r.returncode,'lines',n)
print({b:f'{N-bad[b]}/{N} ok' for b in bins})
