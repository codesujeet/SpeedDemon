# differential fuzz for track B: usage fuzz_bd.py base_bin new_bin [cases]
import random, subprocess, os, sys, hashlib
S='/tmp/claude-0/-home-user-SpeedDemon/41f9d852-ae71-53d3-97e9-45f8193ee723/scratchpad/'
d=S+'edge/bdiff/'; os.makedirs(d,exist_ok=True)
base,new=sys.argv[1],sys.argv[2]; N=int(sys.argv[3]) if len(sys.argv)>3 else 60
rng=random.Random(int(os.environ.get('SEED','7'))); AL='abcdefghijklmnopqrstuvwxyz0123456789'
LENS=[1,2,3,5,8,9,15,16,17,18,24,31,32]
def word(): 
    n=rng.choice(LENS) if rng.random()<.5 else rng.randint(1,12)
    return ''.join(rng.choice(AL) for _ in range(n))
def run(b):
    for f in os.listdir('/out'):
        if f.endswith('.txt'): os.unlink('/out/'+f)
    r=subprocess.run([b],capture_output=True,timeout=60)
    return r.returncode, open('/out/results.txt','rb').read() if os.path.exists('/out/results.txt') else None
bad=0
for case in range(N):
    pool=[word() for _ in range(rng.choice([5,30,300]))]
    qs=[]
    for _ in range(rng.choice([5,50,300])):
        if rng.random()<.4: qs.append('PHRASE "'+' '.join(rng.choice(pool) for _ in range(rng.randint(2,5)))+'"')
        else:
            p=[rng.choice(pool)]
            for _ in range(rng.randint(0,3)): p+=[rng.choice(['AND','OR','NOT']),rng.choice(pool)]
            qs.append('BOOL '+' '.join(p))
    qs+=['BOOL '+word() for _ in range(3000)]     # keeps the term table roomy
    open(d+'queries.txt','w').write('\n'.join(qs)+'\n')
    docs=[]
    for i in range(rng.choice([1,10,200,2000])):
        toks=[]
        for _ in range(rng.randint(0,40)):
            w=rng.choice(pool) if rng.random()<.7 else word()
            if rng.random()<.3: w=w.upper() if rng.random()<.5 else w.capitalize()
            toks.append(w)
        sep=lambda: rng.choice([' ',' ','  ',', ','. ','-','_'])
        t=''
        for w in toks: t+=w+sep()
        docs.append(f'{i}\t{t}')
    open(d+'docs.txt','w').write('\n'.join(docs)+'\n')
    if os.path.islink('/data') or not os.path.exists('/data'):
        if os.path.islink('/data'): os.unlink('/data')
        os.symlink(d.rstrip('/'),'/data')
    a=run(base); b=run(new)
    if a!=b:
        bad+=1; print('DIFF case',case,a[0],b[0],len(a[1] or b''),len(b[1] or b''))
print(f'{N-bad}/{N} identical')
