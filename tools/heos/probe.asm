# probe.asm — raw-syscall TCP prober. No libc.
# usage: ./probe <ip> <port> <payload-file> <timeout-ms>
#   connect, send file contents, poll for reply, print reply, exit.
#   exit 0 = reply received, 1 = timeout (no reply), 2 = connect failed
#
#   as --64 probe.asm -o probe.o && ld probe.o -o probe
#
# syscalls: socket(41) connect(42) sendto(44) recvfrom(45) poll(7)
#           openat(257) read(0) write(1) close(3) exit(60) exit_group(231)

.section .bss
.lcomm sa,      16
.lcomm payload, 8192
.lcomm buf,     8192
.lcomm pfd,     8              # struct pollfd { fd, events, revents }

.section .text
.globl _start

.macro SC n
    movq $\n, %rax
    syscall
.endm

# ---- parse dotted-decimal IP: rdi = string -> eax = addr (network order) ----
# clobbers: rax, rcx, rdx, rsi
parse_ip:
    xorq  %rax, %rax
    xorq  %rcx, %rcx              # current octet value
    xorq  %rdx, %rdx              # shift count (0,8,16,24)
.ip_loop:
    movzbq (%rdi), %rsi
    testq %rsi, %rsi
    jz    .ip_done
    cmpq  $46, %rsi               # '.'
    je    .ip_dot
    # digit: rcx = rcx*10 + (c-'0')
    subq  $48, %rsi
    imulq $10, %rcx
    addq  %rsi, %rcx
    incq  %rdi
    jmp   .ip_loop
.ip_dot:
    # place octet: rax |= rcx << rdx ; rdx += 8
    movq  %rcx, %rsi
    movq  %rdx, %rcx
    shlq  %cl, %rsi
    orq   %rsi, %rax
    addq  $8, %rdx
    xorq  %rcx, %rcx
    incq  %rdi
    jmp   .ip_loop
.ip_done:
    movq  %rcx, %rsi
    movq  %rdx, %rcx
    shlq  %cl, %rsi
    orq   %rsi, %rax
    # NO bswap: octets were assembled low-byte-first, so a direct
    # little-endian store already yields network byte order
    ret

# ---- parse decimal: rdi = string -> rax = value ----
parse_num:
    xorq %rax, %rax
.pn_loop:
    movzbq (%rdi), %rcx
    testq %rcx, %rcx
    jz    .pn_done
    subq  $48, %rcx
    imulq $10, %rax
    addq  %rcx, %rax
    incq  %rdi
    jmp   .pn_loop
.pn_done:
    ret

# ---- strlen: rdi -> rax ----
strlen:
    xorq %rax, %rax
.sl_loop:
    cmpb $0, (%rdi, %rax)
    je   .sl_done
    incq %rax
    jmp  .sl_loop
.sl_done:
    ret

_start:
    # argc check: need prog + 4 args
    movq (%rsp), %rax
    cmpq $5, %rax
    jl   .usage

    # argv[1] = ip string -> parse
    movq 16(%rsp), %rdi
    call parse_ip
    movl %eax, %r12d               # r12d = ip (network order)

    # argv[2] = port -> parse, byteswap to network order
    movq 24(%rsp), %rdi
    call parse_num
    xchgb %al, %ah                 # htons
    movw  %ax, %r13w               # r13w = port (network order)

    # argv[4] = timeout ms -> r14
    movq 40(%rsp), %rdi
    call parse_num
    movq %rax, %r14

    # argv[3] = payload file -> read into payload buf
    movq 32(%rsp), %rdi
    call strlen
    # openat(AT_FDCWD=-100, path, O_RDONLY=0)
    movq $257, %rax
    movq $-100, %rdi
    movq 32(%rsp), %rsi
    xorq %rdx, %rdx
    syscall
    testq %rax, %rax
    js    .file_err
    movq  %rax, %r15               # r15 = fd
    # read(fd, payload, 8192)
    xorq %rax, %rax                # sys_read = 0
    movq %r15, %rdi
    leaq payload(%rip), %rsi
    movq $8192, %rdx
    syscall
    movq %rax, %r13                # r13 = payload len (reuse high bits later)
    pushq %r13                     # save len
    # close(fd)
    movq $3, %rax
    movq %r15, %rdi
    syscall
    popq %r13

    # stash: push ip, port, len (parse results we need later)
    # r12d currently = ip? No — recompute cleanly. Stack layout below.
    # re-parse ip -> stack, port -> stack (registers are scarce)
    movq 16(%rsp), %rdi
    call parse_ip
    pushq %rax                     # [rsp] = ip (network order)
    movq 24(%rsp), %rdi            # careful: rsp shifted by push! argv now at +8
    # FIX: after push, original 24(%rsp) is now 32(%rsp)
    movq 32(%rsp), %rdi
    call parse_num
    xchgb %al, %ah
    pushq %rax                     # [rsp] = port, [rsp+8] = ip
    # payload len already in r13 — push it too
    pushq %r13                     # [rsp] = len, [rsp+8] = port, [rsp+16] = ip

    # socket(AF_INET=2, SOCK_STREAM=1, 0)
    movq $2, %rdi
    movq $1, %rsi
    xorq %rdx, %rdx
    SC 41
    testq %rax, %rax
    js    .conn_err
    movq %rax, %r12                # r12 = sockfd

    # rebuild sockaddr from stack
    movq 0(%rsp), %r13             # r13 = len
    movw $2, sa(%rip)
    movw 8(%rsp), %ax
    movw %ax, sa+2(%rip)
    movl 16(%rsp), %eax
    movl %eax, sa+4(%rip)
    addq $24, %rsp                 # clean stack

    # connect(sockfd, sa, 16)
    movq %r12, %rdi
    leaq sa(%rip), %rsi
    movq $16, %rdx
    SC 42
    testq %rax, %rax
    js    .conn_err

    # send(sockfd, payload, len, 0,0,0)
    movq %r12, %rdi
    leaq payload(%rip), %rsi
    movq %r13, %rdx
    xorq %r10, %r10
    xorq %r8, %r8
    xorq %r9, %r9
    SC 44

    # poll for reply: pfd = { fd, POLLIN=1, 0 }
    movl %r12d, pfd(%rip)
    movw $1, pfd+4(%rip)
    movw $0, pfd+6(%rip)
    movq $7, %rax                  # sys_poll
    leaq pfd(%rip), %rdi
    movq $1, %rsi
    movq %r14, %rdx                # timeout ms
    syscall
    testq %rax, %rax
    jz    .timeout                 # 0 = timeout

    # recv(sockfd, buf, 8192, 0,0,0)
    movq %r12, %rdi
    leaq buf(%rip), %rsi
    movq $8192, %rdx
    xorq %r10, %r10
    xorq %r8, %r8
    xorq %r9, %r9
    SC 45
    movq %rax, %r14
    cmpq $0, %r14
    jle  .timeout

    # write(1, buf, n)
    movq $1, %rdi
    leaq buf(%rip), %rsi
    movq %r14, %rdx
    SC 1
    # exit(0) = reply received
    xorq %rdi, %rdi
    SC 60

.timeout:
    # exit(1) = connected, no reply
    movq $1, %rdi
    SC 60

.conn_err:
    # exit(2) = connect failed
    movq $2, %rdi
    SC 60

.file_err:
    movq $1, %rdi
    leaq ferr(%rip), %rsi
    movq $20, %rdx
    SC 1
    movq $3, %rdi
    SC 60

.usage:
    movq $1, %rdi
    leaq use(%rip), %rsi
    movq $38, %rdx
    SC 1
    movq $3, %rdi
    SC 60

.section .rodata
use:  .ascii "usage: ./probe <ip> <port> <file> <ms>\n"
ferr: .ascii "cannot open payload\n"
