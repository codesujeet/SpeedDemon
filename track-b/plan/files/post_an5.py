import pickle, collections, bisect
d=pickle.load(open('idx.pkl','rb'))
Q,terms,lists,sdf,N,firsts=d['Q'],d['terms'],d['lists'],d['sdf'],d['ndocs'],d['firsts']
nt=len(terms); sampled=7805
in_bool=[False]*nt; anchor=[False]*nt
for k,ts,ops in Q:
    if k=='B':
        for t in ts: in_bool[t]=True
for k,ts,ops in Q:
    if k=='P': anchor[min(ts,key=lambda t:sdf[t])]=True
def cls(i):
    if in_bool[i] and not anchor[i]: return int(sdf[i]*32<=sampled)
    if in_bool[i]: return 2
    if anchor[i]: return 3
    return 4
C=[cls(i) for i in range(nt)]
order=sorted(range(nt),key=lambda i:(C[i],-sdf[i],i))
rank=[0]*nt
for r,i in enumerate(order): rank[i]=r
# units per (thread,bucket)
fb=firsts+[N]
docsets=collections.defaultdict(set); post=collections.Counter()
for i in range(nt):
    if C[i] in (1,2):
        b=rank[i]%32
        for doc in lists[i]:
            t=bisect.bisect_right(firsts,doc)-1
            docsets[(t,b)].add(doc-firsts[t]); post[(t,b)]+=1
units={}; extra=0
for key,s in docsets.items():
    prev=0; e=0
    for doc in sorted(s):
        if doc-prev>=15: e+=1
        prev=doc
    units[key]=post[key]+e; extra+=e
ch=sum((u+32767)//32768 for u in units.values())
print('list postings',sum(post.values()),'extra gap units',extra,'chunks',ch,'max units/bucket',max(units.values()),'min',min(units.values()))
