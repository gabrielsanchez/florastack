-- Run from the repository root after packaging: lua packaging/tests/integrations.lua target/integrations
local built = arg[1] or "target/integrations"
local branding = dofile(built .. "/darktable/florastack/Branding.lua")
local Bridge = dofile("packaging/common/Bridge.lua")
local checks = 0
local function test(name, callback)
    local ok, message = pcall(callback)
    if not ok then error(name .. ": " .. tostring(message)) end
    checks = checks + 1
end

test("shell arguments cannot become shell syntax", function()
    assert(Bridge.quote("/photos/Bee's $HOME `id` & wings.tif", false) == "'/photos/Bee'\\''s $HOME `id` & wings.tif'")
    assert(Bridge.quote("C:\\Bee & wings\\", true) == '"C:\\Bee & wings\\\\"')
    for _, value in ipairs {"bad%PATH%", "bad!NAME!", 'bad"name', "bad^name", "bad\nname", "bad\0name"} do
        assert(not pcall(Bridge.quote, value, true))
    end
end)

test("output reservation skips existing images, sidecars and inputs", function()
    local entries = {["/photos/near-stacked.tif"] = true, ["/photos/near-stacked-2.tif.json"] = true,
        ["/photos/near-stacked-3-inputs"] = true}
    local run = Bridge.reserve("/photos", "near.raw", {
        exists = function(path) return entries[path] end,
        mkdir = function(path) assert(not entries[path]); entries[path] = true; return true end,
    })
    assert(run.output == "/photos/near-stacked-4.tif")
    assert(entries[run.inputs])
    local attempts = 0
    assert(not pcall(Bridge.reserve, "/readonly", "near.raw", {
        exists = function() return false end,
        mkdir = function() attempts = attempts + 1; return false end,
    }))
    assert(attempts == 1)
end)

-- Host mocks exercise actual entry points, including export failure/cancellation and import locks.
-- The files map replaces disk IO; scripts must retain every rendered byte before starting the child.
local files, calls, errors, imported, exit_code, missing_output
local function reset()
    files = {["/bin/stack"] = "exe", ["/temp/near.tif"] = "near pixels", ["/temp/far.tif"] = "far pixels"}
    calls, errors, imported, exit_code, missing_output = {}, {}, nil, 0, false
end
local function execute(command)
    if command:match("^mkdir ") then
        local path = command:match("'([^']+)'$") or command:match('"([^"]+)"$')
        if files[path] then return 1 end
        files[path] = "directory"
        return 0
    end
    local launcher = command:match('^call "([^"]+%.cmd)"$')
    if launcher then
        local batch = files[launcher]
        assert(batch:find("setlocal DisableDelayedExpansion\r\n", 1, true))
        command = assert(batch:match('DisableDelayedExpansion\r\n(.-)\r\nexit /b %%errorlevel%%'))
        assert(command:find('"--depth" "16"', 1, true))
        assert(command:find('"--preset" "builtin:landscape"', 1, true))
    else
        assert(command:find("'--depth' '16'", 1, true))
        assert(command:find("'--preset' 'builtin:landscape'", 1, true))
    end
    calls[#calls + 1] = command
    assert(files["/photos/near-stacked-inputs/000001.tif"] == "near pixels")
    assert(files["/photos/near-stacked-inputs/000002.tif"] == "far pixels")
    if exit_code == 0 and not missing_output then files["/photos/near-stacked.tif"] = "stack pixels" end
    return exit_code
end
local native_open, native_rename = io.open, os.rename
-- The host doubles replace IO only for these tests.
-- luacheck: push ignore 122
io.open = function(path, mode)
    if mode == "wb" then files[path] = "" end
    if files[path] == nil then return nil, "missing" end
    local offset = 1
    return {
        close = function() return true end,
        read = function(_, size)
            if offset > #files[path] then return nil end
            local block = files[path]:sub(offset, offset + size - 1)
            offset = offset + #block
            return block
        end,
        write = function(self, block) files[path] = files[path] .. block; return self end,
    }
end
os.rename = function(path) return files[path] ~= nil or nil, "missing", 2 end
-- luacheck: pop

local in_lock = false
local source = {getRawMetadata = function() return "/photos/near.raw" end}
local modules = {
    LrApplication = {activeCatalog = function() return {
        withWriteAccessDo = function(_, _, callback) in_lock = true; callback(); in_lock = false end,
        addPhoto = function(_, path, photo, position)
            assert(in_lock and photo == source and position == "below")
            imported = path
            return {path = path}
        end,
    } end},
    LrDialogs = {message = function(_, message) errors[#errors + 1] = message end},
    LrFileUtils = {
        exists = function(path) return files[path] and "file" or false end,
        copy = function(from, to) files[to] = files[from]; return true end,
    },
    LrPathUtils = {
        isAbsolute = function(path) return path:sub(1, 1) == "/" end,
        parent = function() return "/photos" end,
        leafName = function() return "near.raw" end,
    },
    LrTasks = {pcall = pcall, execute = function(command) assert(not in_lock); return execute(command) end},
    LrView = {},
}
import = function(name) return assert(modules[name], name) end
WIN_ENV = false
package.loaded.Branding, package.loaded.Bridge = branding, Bridge
local provider = dofile("packaging/lightroom/ExportServiceProvider.lua")
local function lightroom(fail_at, canceled)
    local paths = {"/temp/near.tif", "/temp/far.tif"}
    local position = 0
    local progress = {isCanceled = function() return canceled end, setCaption = function() end, done = function() end}
    provider.processRenderedPhotos(nil, {
        propertyTable = {executable = "/bin/stack", preset = "builtin:landscape", method = "pyramid"},
        configureProgress = function() return progress end,
        renditions = function() return function()
            position = position + 1
            if paths[position] then return position, {
                photo = source,
                waitForRender = function() return position ~= fail_at, paths[position] end,
            } end
        end end,
    })
end
test("Lightroom forces 16-bit TIFF and imports only after every render and CLI success", function()
    reset()
    local settings = {LR_format = "JPEG", LR_tiff_bitDepth = 8, LR_useWatermark = true}
    provider.updateExportSettings(settings)
    assert(settings.LR_format == "TIFF" and settings.LR_tiff_bitDepth == 16 and not settings.LR_useWatermark)
    lightroom()
    assert(#calls == 1 and #errors == 0 and imported == "/photos/near-stacked.tif")
end)
test("Lightroom rejects failed exports, cancellation, CLI failures and missing outputs", function()
    reset(); lightroom(2); assert(#calls == 0 and not imported and #errors == 1)
    reset(); lightroom(nil, true); assert(#calls == 0 and not imported)
    reset(); exit_code = 1; lightroom(); assert(#calls == 1 and not imported and #errors == 1)
    reset(); missing_output = true; lightroom(); assert(#calls == 1 and not imported and #errors == 1)
end)

local storage
local dt = {
    configuration = {running_os = "linux"},
    preferences = {
        register = function() end,
        read = function(_, key) return ({executable = "/bin/stack", preset = "builtin:landscape", method = "pyramid"})[key] end,
    },
    control = {execute = execute},
    print_error = function(message) errors[#errors + 1] = message end,
    print = function() end,
    register_storage = function(_, _, _, finalize, supported, initialize)
        storage = {finalize = finalize, supported = supported, initialize = initialize}
    end,
    database = {import = function(path)
        imported = path
        return {group_with = function(_, photo) assert(photo.filename == "near.raw") end}
    end},
}
package.loaded.darktable = dt
package.loaded["florastack.Branding"], package.loaded["florastack.Bridge"] = branding, Bridge
dofile("packaging/darktable/florastack.lua")
local near = {path = "/photos", filename = "near.raw", id = 1}
local far = {path = "/photos", filename = "z-far.raw", id = 2}
local function darktable(incomplete, format)
    local data = {}
    format = format or {extension = "tif", bpp = 8, max_width = 500, max_height = 500}
    storage.initialize(nil, format, {far, near}, false, data)
    local rendered = {[near] = "/temp/near.tif"}
    if not incomplete then rendered[far] = "/temp/far.tif" end
    storage.finalize(nil, rendered, data)
    return format
end
test("darktable forces full-size 16-bit TIFF and uses original order despite shuffled export completion", function()
    reset()
    assert(not storage.supported(nil, {extension = "jpg"}))
    local format = darktable()
    assert(format.bpp == 16 and format.max_width == 0 and format.max_height == 0)
    assert(#calls == 1 and imported == "/photos/near-stacked.tif" and #errors == 0)
end)
test("darktable rejects other formats, incomplete exports, CLI failures and missing outputs", function()
    reset(); darktable(false, {extension = "jpg"}); assert(#calls == 0 and not imported and #errors == 1)
    reset(); darktable(true); assert(#calls == 0 and not imported and #errors == 1)
    reset(); exit_code = 1; darktable(); assert(#calls == 1 and not imported and #errors == 1)
    reset(); missing_output = true; darktable(); assert(#calls == 1 and not imported and #errors == 1)
end)

test("darktable Windows passes one quoted batch path to the host and preserves CLI failures", function()
    dt.configuration.running_os = "windows"
    dofile("packaging/darktable/florastack.lua")
    reset(); darktable(); assert(#calls == 1 and imported and #errors == 0)
    assert(files["/photos/near-stacked-inputs/stack.cmd"])
    reset(); exit_code = 1; darktable(); assert(#calls == 1 and not imported and #errors == 1)
end)

io.open, os.rename = native_open, native_rename -- luacheck: ignore 122
print(string.format("Editor integration checks: %d passed", checks))
