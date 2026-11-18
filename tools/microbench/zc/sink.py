import socket,threading
s=socket.socket(); s.setsockopt(socket.SOL_SOCKET,socket.SO_REUSEADDR,1); s.bind(("10.42.0.2",9100)); s.listen(8)
def h(c):
    b=bytearray(1<<22)
    while c.recv_into(b): pass
while True:
    c,_=s.accept(); threading.Thread(target=h,args=(c,),daemon=True).start()
