add_rules("mode.debug", "mode.release")

target("kernel")
    set_kind("static")
    on_build(function (target)
        os.cd("kernel")
        if is_mode("release") then
            os.exec("cargo build  --release")
        else
            os.exec("cargo build")
        end
    end)
    after_build(function (target)
        if os.exists("kernel.exe") then
            os.rm("kernel.exe")
        end
        if is_mode("release") then
            os.cp("kernel/target/i686-pc-windows-msvc/release/kernel.exe", "kernel.exe")
        else
            os.cp("kernel/target/i686-pc-windows-msvc/debug/kernel.exe", "kernel.exe")
        end
    end)

target("loader")
    set_kind("phony")
    on_build(function (target)
        os.exec("nasm -o loader.bin loader.s -l loader.lst")
    end)
    add_deps("kernel")


target("disk")
    set_kind("phony")
    before_build(function (target)
        os.exec("python extract.py")
        print("Restructed Kernel Size: %d", os.filesize("kernel.bin"))
    end)
    on_build(function (target)
        os.exec("nasm -o disk.img disk.s -l disk.lst")
        print("Loader and Kernel Size: %d", os.filesize("loader.bin"))
    end)
    add_deps("loader")
    

target("rondos")
    set_kind("phony")
    on_run(function (target)
        print("run")
        os.exec("bochs -f bochsrc.bxrc -q")
    end)
    add_deps("disk")

--
-- If you want to known more usage about xmake, please see https://xmake.io
--
-- ## FAQ
--
-- You can enter the project directory firstly before building project.
--
--   $ cd projectdir
--
-- 1. How to build project?
--
--   $ xmake
--
-- 2. How to configure project?
--
--   $ xmake f -p [macosx|linux|iphoneos ..] -a [x86_64|i386|arm64 ..] -m [debug|release]
--
-- 3. Where is the build output directory?
--
--   The default output directory is `./build` and you can configure the output directory.
--
--   $ xmake f -o outputdir
--   $ xmake
--
-- 4. How to run and debug target after building project?
--
--   $ xmake run [targetname]
--   $ xmake run -d [targetname]
--
-- 5. How to install target to the system directory or other output directory?
--
--   $ xmake install
--   $ xmake install -o installdir
--
-- 6. Add some frequently-used compilation flags in xmake.lua
--
-- @code
--    -- add debug and release modes
--    add_rules("mode.debug", "mode.release")
--
--    -- add macro defination
--    add_defines("NDEBUG", "_GNU_SOURCE=1")
--
--    -- set warning all as error
--    set_warnings("all", "error")
--
--    -- set language: c99, c++11
--    set_languages("c99", "c++11")
--
--    -- set optimization: none, faster, fastest, smallest
--    set_optimize("fastest")
--
--    -- add include search directories
--    add_includedirs("/usr/include", "/usr/local/include")
--
--    -- add link libraries and search directories
--    add_links("tbox")
--    add_linkdirs("/usr/local/lib", "/usr/lib")
--
--    -- add system link libraries
--    add_syslinks("z", "pthread")
--
--    -- add compilation and link flags
--    add_cxflags("-stdnolib", "-fno-strict-aliasing")
--    add_ldflags("-L/usr/local/lib", "-lpthread", {force = true})
--
-- @endcode
--

