import pickle, collections
d=pickle.load(open('idx.pkl','rb'))
Q,terms,lists,sdf,N=d['Q'],d['terms'],d['lists'],d['sdf'],d['ndocs']
nt=len(terms); sampled=7805
in_bool=[False]*nt; anchor=[False]*nt
for k,ts,ops in Q:
    if k=='B':
        for t in ts: in_bool[t]=True
for k,ts,ops in Q:
    if k=='P': anchor[min(ts,key=lambda t:sdf[t])]=True
isbits=[in_bool[i] and not anchor[i] and sdf[i]*32>sampled for i in range(nt)]
arms=collections.Counter(); armsz=collections.defaultdict(int)
docs_calls=0; docs_elems=0; skipped=0
res=collections.Counter(); nums=0; digits=0; listsum_elems=0; bitres=0
qtypes=collections.Counter(); nterms_b=collections.Counter()
firstbits=0
for k,ts,ops in Q:
    if k=='P':
        qtypes['P']+=1; continue
    qtypes['B']+=1; nterms_b[len(ts)]+=1
    def docs(i):
        global docs_calls, docs_elems
        if isbits[i]: return ('B',set(lists[i]))
        docs_calls+=1; docs_elems+=len(lists[i]); return ('L',set(lists[i]))
    cur=docs(ts[0])
    if cur[0]=='B': firstbits+=1
    for op,t in zip(ops,ts[1:]):
        if cur[0]=='L' and not cur[1] and op!=b'O':
            skipped+=1; continue
        tm=docs(t)
        a,b=cur,tm
        def ap(a,op,b):
            A,B=a[1],b[1]
            r = A&B if op==b'A' else (A|B if op==b'O' else A-B)
            if a[0]=='B' and b[0]=='B': arms['BB']+=1; return ('B',r)
            if (a[0]=='B' and op==b'A' and b[0]=='L') or (a[0]=='L' and op in b'AN' and b[0]=='B'):
                arms['retain']+=1; armsz['retain']+=len(B if a[0]=='B' else A); return ('L',r)
            if a[0]=='B' or b[0]=='B':
                arms['setbits_'+op.decode()]+=1; armsz['setbits_'+op.decode()]+=len(B if a[0]=='B' else A); return ('B',r)
            la,lb=len(A),len(B)
            if op in b'AN' and la*16<lb:
                arms['bsearch']+=1; armsz['bsearch']+=la; return ('L',r)
            if la+lb > (2048 if op==b'O' else 768):
                arms['tobits_'+op.decode()]+=1; armsz['tobits_'+op.decode()]+=lb
                return ap(a,op,('B',B))
            arms['merge_'+op.decode()]+=1; armsz['merge_'+op.decode()]+=la+lb; return ('L',r)
        cur=ap(a,op,b)
    res[cur[0]]+=1
    if cur[0]=='L': listsum_elems+=len(cur[1])
    n=len(cur[1]); s=sum(cur[1]); ids=sorted(cur[1])[:10]
    for x in [n,s]+ids: nums+=1; digits+=len(str(x))
# phrase lines
for k,ts,ops in Q:
    pass
print(qtypes, sorted(nterms_b.items()))
print('first term bitmap',firstbits,'docs() calls',docs_calls,'elems',docs_elems,'skipped',skipped)
for a in sorted(arms): print(a,arms[a],armsz[a])
print('results',res,'listsum elems',listsum_elems,'bool nums',nums,'digits',digits)
