import pefile
pe = pefile.PE('kernel.exe')

with open('kernel.inc', 'w+') as inc:
    print("Entry: ", hex(pe.OPTIONAL_HEADER.AddressOfEntryPoint))
    inc.write('%define KERNEL_ENTRY ' + hex(pe.OPTIONAL_HEADER.AddressOfEntryPoint))

with open('kernel.bin', 'wb+') as f:
    curaddr = 0x1000
    for section in pe.sections:
        name = section.Name
        vaddr = section.VirtualAddress
        size = section.SizeOfRawData
        data = section.get_data()
        print('Name:', name,'VAddr:', hex(vaddr), 'Size:', hex(size))
        if curaddr < vaddr:
            f.write(b"\0" * (vaddr - curaddr))
        f.write(data)
        curaddr += size
