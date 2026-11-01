import pickle, collections, bisect, array
d=pickle.load(open('idx.pkl','rb'))
Q,terms,lists,N,firsts,cut=d['Q'],d['terms'],d['lists'],d['ndocs'],d['firsts'],d['cut']
nt=len(terms)
text=open('/tmp/claude-0/bpost/data/docs.txt','rb').read()
def sample_docs(frac):
    ranges=[]; tot=0
    for t in range(12):
        end=cut[t]+(cut[t+1]-cut[t])//frac
        n=text.count(b'\n',cut[t],end)+(0 if text[end-1]==10 else 1)
        ranges.append((firsts[t],firsts[t]+n)); tot+=n
    return ranges,tot
def run(frac):
    ranges,sampled=sample_docs(frac)
    sdf=[0]*nt
    for i,l in enumerate(lists):
        c=0
        for a,b in ranges:
            c+=bisect.bisect_left(l,b)-bisect.bisect_left(l,a)
        sdf[i]=c
    in_bool=[False]*nt; anchor=[False]*nt
    for k,ts,ops in Q:
        if k=='B':
            for t in ts: in_bool[t]=True
    work=0; opt=0; worse=0
    for k,ts,ops in Q:
        if k=='P':
            a=min(ts,key=lambda t:sdf[t]); anchor[a]=True
            work+=len(lists[a]); o=min(len(lists[t]) for t in ts); opt+=o; worse+= len(lists[a])>o
    isb=[in_bool[i] and not anchor[i] and sdf[i]*32>sampled for i in range(nt)]
    nb=sum(isb); lp=sum(len(lists[i]) for i in range(nt) if in_bool[i] and not isb[i])
    bp=sum(len(lists[i]) for i in range(nt) if isb[i])
    print(f'1/{frac}: sampled {sampled} bitmaps {nb} list-postings {lp} bitmap-postings {bp} anchor df-sum {work} (optimal {opt}), phrases with non-rarest anchor {worse}')
for f in (32,64,128): run(f)
