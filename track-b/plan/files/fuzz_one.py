import random, subprocess, os, sys, hashlib
D='/tmp/claude-0/bpost/fz/'
rng=random.Random(int(sys.argv[1]) if len(sys.argv)>1 else 1)
AL='abcdefghijklmnopqrstuvwxyz0123456789'
def mkword(): return ''.join(rng.choice(AL) for _ in range(rng.choice([1,2,3,4,5,8,12,17])))
bad=0; N=int(sys.argv[2]) if len(sys.argv)>2 else 12
for case in range(N):
    stop = case==4
    pool=[mkword() for _ in range(rng.choice([4,20,200,2000]))]
    w=[1/(i+1) for i in range(len(pool))]
    nd=rng.choice([1,3,40,600,5000,30000])
    lines=[]
    for i in range(nd):
        k=rng.random()
        if k<0.03: txt=''
        elif k<0.05: txt='§§ * —'
        else:
            n=rng.choice([1,3,10,80,300])
            ws=rng.choices(pool,w,k=n)
            if rng.random()<0.1: ws=[x.upper() for x in ws]
            seps=[rng.choice([' ',' ',' ',', ','-','—',' ','. ',"'s "]) for _ in ws]
            txt=''.join(a+b for a,b in zip(ws,seps))
            if rng.random()<0.05: txt+=' Zürich café 007'
        lines.append(f'{i}\t{txt}\n')
    open(D+'docs.txt','w',encoding='utf-8').write(''.join(lines))
    qs=[]
    for _ in range(rng.choice([50,400,3000])):
        if rng.random()<0.38:
            n=rng.randint(2,8)
            if rng.random()<0.7 and nd>0:
                # window from a doc
                doc=rng.randrange(nd); import re
                toks=re.findall(r'[a-z0-9]+',lines[doc].split('\t',1)[1].encode('utf-8').lower().decode('latin1'))
                if len(toks)>=n:
                    s=rng.randrange(len(toks)-n+1); qs.append('PHRASE "'+' '.join(toks[s:s+n])+'"'); continue
            qs.append('PHRASE "'+' '.join(rng.choices(pool,w,k=n))+'"')
        else:
            n=rng.randint(1,6); ts=rng.choices(pool+['zurich','nosuchterm','and','or','not'],k=n)
            q='BOOL '+ts[0]
            for t in ts[1:]: q+=' '+rng.choice(['AND','AND','OR','NOT'])+' '+t
            qs.append(q)
    qs+=['BOOL a']*2000
    open(D+'queries.txt','w').write('\n'.join(qs)+'\n')
    if not stop:
        continue
    outs=[]
    for v in ['b0','bF']:
        o=D+f'out_{v}.txt'
        if os.path.exists(o): os.unlink(o)
        r=subprocess.run(['valgrind','-q','--tool=none',f'/tmp/claude-0/bpost/f_{v}/target/release/track-b'],capture_output=True,timeout=600)
        outs.append((r.returncode, open(o,'rb').read() if os.path.exists(o) else None))
    same = outs[0]==outs[1] and outs[0][1] is not None
    print(r.stderr.decode()[-600:]); break
    if not same: bad+=1; print('DIFF case',case,nd,[x[0] for x in outs])
    else: print('ok case',case,'docs',nd,'queries',len(qs),'sha',hashlib.sha256(outs[0][1]).hexdigest()[:10])

