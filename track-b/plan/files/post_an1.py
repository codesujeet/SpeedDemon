import re, sys, pickle, collections, array
D='/tmp/claude-0/-home-user-SpeedDemon/41f9d852-ae71-53d3-97e9-45f8193ee723/scratchpad/data/b_real/'
qs=open(D+'queries.txt','rb').read().split(b'\n')
qs=[q for q in qs if q]
Q=[]
terms={}
def tid(w):
    if w not in terms: terms[w]=len(terms)
    return terms[w]
for q in qs:
    if q[0:1]==b'P':
        ws=q[8:-1].split(b' ')
        Q.append(('P',[tid(w) for w in ws],[]))
    else:
        ws=q[5:].split(b' ')
        Q.append(('B',[tid(w) for w in ws[0::2]],[w[0:1] for w in ws[1::2]]))
print('queries',len(Q),'distinct terms',len(terms),'words',sum(len(q[1]) for q in Q))
text=open(D+'docs.txt','rb').read()
T=12; L=len(text)
cut=[L*t//T for t in range(T+1)]
for t in range(1,T):
    c=cut[t]
    while c<L and text[c-1]!=10: c+=1
    cut[t]=c
lists=[array.array('I') for _ in terms]
sdf=[0]*len(terms)
rx=re.compile(rb'[a-z0-9]+')
doc=0; pos=0
firsts=[]
samp_end=[cut[t]+(cut[t+1]-cut[t])//32 for t in range(T)]
th=0
ntok=0
while pos<L:
    while th<T and pos>=cut[th]:
        firsts.append(doc); th+=1
    e=text.index(b'\n',pos)
    tab=text.index(b'\t',pos)
    toks=rx.findall(text[tab+1:e].lower())
    ntok+=len(toks)
    s=set(toks)
    insamp = pos < samp_end[th-1]
    for w in s:
        i=terms.get(w)
        if i is not None:
            lists[i].append(doc)
            if insamp: sdf[i]+=1
    doc+=1; pos=e+1
print('ndocs',doc,'tokens',ntok,'firsts',firsts)
pickle.dump(dict(Q=Q,terms=terms,lists=lists,sdf=sdf,firsts=firsts,ndocs=doc,cut=cut),open('/tmp/claude-0/bpost/idx.pkl','wb'))
