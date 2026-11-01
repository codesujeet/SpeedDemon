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
term_elems=collections.Counter(); term_calls=collections.Counter()
first=[0,0]
emptyOR=collections.Counter()
single=collections.Counter()
for k,ts,ops in Q:
    if k=='P': continue
    if not isbits[ts[0]]: first[0]+=1; first[1]+=len(lists[ts[0]])
    cur=('B' if isbits[ts[0]] else 'L', set(lists[ts[0]]))
    if len(ts)==1: single[cur[0]]+=1
    for op,t in zip(ops,ts[1:]):
        if cur[0]=='L' and not cur[1] and op!=b'O': continue
        b=('B' if isbits[t] else 'L', set(lists[t]))
        A,B=cur[1],b[1]
        r = A&B if op==b'A' else (A|B if op==b'O' else A-B)
        la,lb=len(A),len(B)
        if cur[0]=='L' and not A and op==b'O': emptyOR[b[0]]+=1
        if b[0]=='L':
            if cur[0]=='B': arm='retain_term' if op==b'A' else 'setbits_term_'+op.decode()
            elif op in b'AN' and la*16<lb: arm='bsearch_term'
            elif la+lb > (2048 if op==b'O' else 768): arm='tobits_term_'+op.decode()
            else: arm='merge_term_'+op.decode()
            term_elems[arm]+=lb; term_calls[arm]+=1
        if cur[0]=='B' and b[0]=='B': nk='B'
        elif cur[0]=='B' and op==b'A': nk='L'
        elif cur[0]=='L' and op in b'AN' and b[0]=='B': nk='L'
        elif cur[0]=='B' or b[0]=='B': nk='B'
        elif op in b'AN': nk='L'
        elif la+lb>2048: nk='B'
        else: nk='L'
        cur=(nk,r)
print('first-term list docs()',first)
for a in sorted(term_elems): print(a,term_calls[a],term_elems[a])
print('sum later',sum(term_elems.values()))
print('empty cur then OR (term kind):',emptyOR,'single',single)
