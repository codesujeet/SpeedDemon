import random, subprocess, os, sys
S='/tmp/claude-0/-home-user-SpeedDemon/41f9d852-ae71-53d3-97e9-45f8193ee723/scratchpad/'
BOM=b'\xef\xbb\xbf'
def gen(rng):
    nreg=rng.randint(1,60); ids=sorted(rng.sample(range(1,rng.choice([100,5000,999999])),nreg))
    countries=[''.join(rng.choice('ABCDEFGHIJKLMNOPQRSTUVWXYZ') for _ in range(3)) for _ in range(rng.randint(1,8))]
    regions=[(i,rng.choice(countries)) for i in ids]
    eol_reg=b'\n'
    reg=b'region_id,country\n'+b''.join(b'%d,%s\n'%(i,c.encode()) for i,c in regions)
    out=BOM+b'id,name,note,region_id,amount\r\n'
    words=['Ann','Betts','Kaphar','Titus','x','Zed','Lee']
    id=0; recs=[]; prev_bom=True
    for r in range(rng.randint(1,200)):
        id+=rng.choice([1,1,1,1,2,3,11]) if r else rng.choice([1,5,99,1000])
        def name():
            k=rng.random()
            if k<0.05: return b''
            if k<0.1: return b'""'
            s=' '.join(rng.choice(words) for _ in range(rng.randint(1,3)))
            if rng.random()<0.3: s+=' '*rng.randint(1,6)
            if rng.random()<0.3: s=s.replace(' ',', ',1)
            if rng.random()<0.3: s='%s "%s" %s'%(s[:2],rng.choice(words),s[2:])
            return b'"'+s.replace('"','""').encode()+b'"'
        def note():
            k=rng.random()
            if k<0.05: return b''
            if k<0.1: return b'""'
            parts=[' '.join(rng.choice(['one','two','said','no','twice','a']) for _ in range(rng.randint(1,4))) for _ in range(rng.randint(1,4))]
            s='\n'.join(parts)
            if rng.random()<0.3: s='"'+s
            if rng.random()<0.3: s+='"'
            if rng.random()<0.2: s+=' '
            return b'"'+s.replace('"','""').encode()+b'"'
        k=rng.random()
        region=rng.choice(ids) if k<0.7 else (rng.randint(1,ids[-1]+5) if k<0.9 else rng.randint(ids[-1]+1,9999999))
        c=rng.choice([0,0,rng.randint(0,99999)])*100+rng.randint(0,99) if rng.random()<0.9 else 0
        if rng.random()<0.15: c=rng.randint(0,99)
        amt=('-' if rng.random()<0.2 and c else '')+'%d.%02d'%(c//100,c%100)
        recs.append((id,name(),note(),region,amt))
    body=b''; 
    for n,(id,nm,nt,rg,am) in enumerate(recs):
        bom = (not prev_bom and n>0 and rng.random()<0.1) 
        body+= (BOM if bom else b'')
        prev_bom = bom if n>0 else False
        body+=b'%d,%s,%s,%d,%s'%(id,nm,nt,rg,am.encode())
        term=rng.choice([b'\r\n',b'\n']) 
        if n==len(recs)-1 and rng.random()<0.5: term=b'\r\n'
        body+=term
        if term in (b'\r\n',b'\n'): prev_bom=False if not bom else prev_bom
    return out+body, reg, regions, recs
def oracle(rec_bytes, regions):
    cmap=dict(regions)
    i=rec_bytes.index(b'\n')+1   # after header
    d={}
    n=len(rec_bytes)
    while i<n:
        if rec_bytes[i:i+3]==BOM: i+=3
        j=rec_bytes.index(b',',i); id=int(rec_bytes[i:j]); i=j+1
        fields=[]
        for _ in range(2):
            if rec_bytes[i:i+1]==b'"':
                i+=1; buf=bytearray()
                while True:
                    if rec_bytes[i:i+2]==b'""': buf.append(34); i+=2
                    elif rec_bytes[i:i+1]==b'"': i+=1; break
                    else: buf.append(rec_bytes[i]); i+=1
                fields.append(bytes(buf)); assert rec_bytes[i:i+1]==b','; i+=1
            else:
                j=rec_bytes.index(b',',i); fields.append(rec_bytes[i:j]); i=j+1
        j=rec_bytes.index(b',',i); region=int(rec_bytes[i:j]); i=j+1
        k=i
        while k<n and rec_bytes[k] not in (10,13): k+=1
        amt=rec_bytes[i:k].decode(); neg=amt.startswith('-'); a=amt.lstrip('-'); ip,fp=a.split('.'); cents=int(ip)*100+int(fp); cents=-cents if neg else cents
        i=k+ (2 if rec_bytes[k:k+2]==b'\r\n' else 1)
        c=cmap.get(region,'_UNMATCHED')
        e=d.setdefault(c,dict(n=0,ids=0,sum=0,mn=None,mx=None,names=set(),nb=0,tb=0))
        e['n']+=1; e['ids']+=id; e['sum']+=cents; e['mn']=cents if e['mn'] is None else min(e['mn'],cents); e['mx']=cents if e['mx'] is None else max(e['mx'],cents)
        e['names'].add(fields[0]); e['nb']+=len(fields[0]); e['tb']+=len(fields[1])
    def money(c): return ('-' if c<0 else '')+'%d.%02d'%(abs(c)//100,abs(c)%100)
    lines=[]
    for c in sorted(x for x in d if x!='_UNMATCHED')+(['_UNMATCHED'] if '_UNMATCHED' in d else []):
        e=d[c]; lines.append('%s %d %d %s %s %s %d %d %d\n'%(c,e['n'],e['ids'],money(e['sum']),money(e['mn']),money(e['mx']),len(e['names']),e['nb'],e['tb']))
    return ''.join(lines).encode()
bins=sys.argv[2:]; N=int(sys.argv[1]); rng=random.Random(12345); bad={b:0 for b in bins}
os.makedirs(S+'edge/fuzz',exist_ok=True)
for case in range(N):
    rec,reg,regions,_=gen(rng)
    if case%3==0: reg=reg.replace(b'\n',b'\r\n')     # CRLF region files too
    open(S+'edge/fuzz/records.csv','wb').write(rec); open(S+'edge/fuzz/regions.csv','wb').write(reg)
    want=oracle(rec,regions)
    for b in bins:
        try: os.unlink('/out/aggregates.txt')
        except FileNotFoundError: pass
        r=subprocess.run([S+'g15/'+b],capture_output=True)
        got=open('/out/aggregates.txt','rb').read() if os.path.exists('/out/aggregates.txt') else b'<none>'
        if r.returncode!=0 or got!=want:
            bad[b]+=1
            if bad[b]<=2: print('MISMATCH',b,'case',case,'rc',r.returncode,'crlf-regions' if case%3==0 else '')
print({b:f'{N-bad[b]}/{N} ok' for b in bins})
