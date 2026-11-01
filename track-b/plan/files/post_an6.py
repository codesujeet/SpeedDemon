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
S=[None]*nt
def st(i):
    if S[i] is None: S[i]=set(lists[i])
    return S[i]
origin=collections.Counter(); bitsizes=[]; emptyor_big=0; emptyor_bits_final=0
swap=collections.Counter(); swapsz=collections.Counter()
lastop_tobitsO=0; lastop_sizes=[]
for k,ts,ops in Q:
    if k=='P': continue
    kind='B' if isbits[ts[0]] else 'L'; cur=st(ts[0]); org='first-bitmap' if kind=='B' else None
    emptyflag=False
    for n,(op,t) in enumerate(zip(ops,ts[1:])):
        if kind=='L' and not cur and op!=b'O': continue
        bk='B' if isbits[t] else 'L'; B=st(t)
        la,lb=len(cur),len(B)
        ef = (kind=='L' and la==0 and op==b'O')
        r = cur&B if op==b'A' else (cur|B if op==b'O' else cur-B)
        if kind=='B' and bk=='B': nk='B'; o='BB'
        elif kind=='B' and op==b'A': nk='L'
        elif kind=='L' and op in b'AN' and bk=='B': nk='L'
        elif kind=='B' or bk=='B': nk='B'; o='setbits_'+op.decode()+('_emptycur' if ef else '')
        elif op in b'AN':
            nk='L'
            if op==b'A' and lb*16<la and not la*16<lb:
                arm='tobits' if la+lb>768 else 'merge'
                swap[arm]+=1; swapsz[arm]+=la+lb
        elif la+lb>2048: nk='B'; o='tobitsO'+('_emptycur' if ef else '')+('_last' if n==len(ops)-1 else '')
        else: nk='L'
        if nk=='B' and not (kind=='B'): org=o
        elif nk=='B' and o=='BB': org='BB'
        elif nk=='B': org=org if org else o
        if nk=='L': org=None
        kind=nk; cur=r
    if kind=='B':
        origin[org]+=1; bitsizes.append(len(cur))
print('Bits results by creating op:',origin.most_common())
bs=sorted(bitsizes); import statistics
print('bits result sizes: n',len(bs),'min',bs[0],'p10',bs[len(bs)//10],'median',bs[len(bs)//2],'p90',bs[9*len(bs)//10],'max',bs[-1])
print('AND list-list with b*16<a (swap candidates):',swap,swapsz)
print('bits results with count<10:',sum(1 for x in bitsizes if x<10),' count==0:',sum(1 for x in bitsizes if x==0), ' <100:', sum(1 for x in bitsizes if x<100))
