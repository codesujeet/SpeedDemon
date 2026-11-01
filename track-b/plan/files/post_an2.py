import pickle, collections, bisect
d=pickle.load(open('idx.pkl','rb'))
Q,terms,lists,sdf,firsts,N=d['Q'],d['terms'],d['lists'],d['sdf'],d['firsts'],d['ndocs']
nt=len(terms)
# sampled docs count
cut=d['cut']; text=None
import array
sampled=0
# recompute sampled from firsts & sample ends: approximate by counting lists? do directly
D='/tmp/claude-0/-home-user-SpeedDemon/41f9d852-ae71-53d3-97e9-45f8193ee723/scratchpad/data/b_real/'
text=open(D+'docs.txt','rb').read()
for t in range(12):
    end=cut[t]+(cut[t+1]-cut[t])//32
    sampled+=text.count(b'\n',cut[t],end)+ (0 if text[end-1]==10 else 1)
print('sampled',sampled,'nterms',nt)
in_bool=[False]*nt; anchor=[False]*nt
for k,ts,ops in Q:
    if k=='B':
        for t in ts: in_bool[t]=True
for k,ts,ops in Q:
    if k=='P':
        a=min(ts,key=lambda t:sdf[t]); anchor[a]=True
def cls(i):
    if in_bool[i] and not anchor[i]: return int(sdf[i]*32<=sampled)
    if in_bool[i]: return 2
    if anchor[i]: return 3
    return 4
C=[cls(i) for i in range(nt)]
cnt=collections.Counter(C); print('classes',sorted(cnt.items()))
df=[len(l) for l in lists]
for c in range(5):
    ids=[i for i in range(nt) if C[i]==c]
    print('class',c,'terms',len(ids),'postings',sum(df[i] for i in ids),'max df',max((df[i] for i in ids),default=0))
# units: per thread, per bucket gap stats approx: count postings with gap>=15 per term-bucket (bucket=rank%32)
order=sorted(range(nt),key=lambda i:(C[i],-sdf[i],i))
new=[0]*nt
for r,i in enumerate(order): new[i]=r
nbits=cnt[0]; lists_n=cnt[0]+cnt[1]+cnt[2]
print('nbits',nbits,'lists',lists_n,'nkeep',lists_n+cnt[3])
# Units cost: per thread, per bucket stream of docs ordered by doc; gap = doc - prev doc in that bucket stream (per doc unique)
import numpy as _np
