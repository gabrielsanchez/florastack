-- Uses only darktable's documented Lua API; no darktable implementation or script code is incorporated.
local dt = require "darktable"
local branding = require "florastack.Branding"
local Bridge = require "florastack.Bridge"
local namespace = branding.app_id .. ".stack"
local windows = dt.configuration.running_os == "windows"

dt.preferences.register(namespace, "executable", "file", branding.name .. " CLI", "Absolute path to " .. branding.cli, "")
dt.preferences.register(namespace, "preset", "string", branding.name .. " preset", "builtin:<id> or an absolute TOML path", "builtin:insect-macro")
dt.preferences.register(namespace, "method", "string", branding.name .. " method", "pyramid, depthmap or average", "pyramid")

local function exists(path)
    local file = io.open(path, "rb")
    if file then file:close(); return true end
    -- os.rename(path, path) also detects directories (and permission-denied entries).
    local ok, _, code = os.rename(path, path)
    return ok == true or code == 13
end

local host = {
    windows = windows,
    exists = exists,
    execute = dt.control.execute,
    mkdir = function(path)
        local command = (windows and "mkdir " or "mkdir -- ") .. Bridge.quote(path, windows)
        return dt.control.execute(command) == 0
    end,
}

local function report(message)
    dt.print_error(branding.name .. ": " .. tostring(message))
    dt.print(branding.name .. ": " .. tostring(message))
end

local function supported(_, format)
    return format.extension == "tif" or format.extension == "tiff"
end

local function initialize(_, format, images, _, data)
    local ok, message = pcall(function()
        if not supported(nil, format) then error("Choose TIFF as the export format.") end
        if #images < 2 then error("Select at least two still photos in filename focus order.") end
        data.settings = {}
        for _, key in ipairs {"executable", "preset", "method"} do
            data.settings[key] = dt.preferences.read(namespace, key, key == "executable" and "file" or "string")
        end
        Bridge.check(data.settings, windows)
        if not exists(data.settings.executable) then error("Set the installed CLI path in Preferences > Lua options.") end
        -- Export completion order is unrelated to focus order. Snapshot and sort the original paths.
        data.images = {}
        for index, image in ipairs(images) do data.images[index] = image end
        table.sort(data.images, function(a, b)
            local left, right = Bridge.join(a.path, a.filename), Bridge.join(b.path, b.filename)
            if left == right then return a.id < b.id end
            return left < right
        end)
        format.bpp = 16
        format.max_width = 0
        format.max_height = 0
        local first = data.images[1]
        data.run = Bridge.reserve(first.path, first.filename, host)
    end)
    if not ok then data.failed = true; report(message); return {} end
    return data.images
end

local function copy(source, destination)
    local input, message = io.open(source, "rb")
    if not input then error("Could not read rendered frame: " .. tostring(message)) end
    local output
    output, message = io.open(destination, "wb")
    if not output then input:close(); error("Could not retain frame: " .. tostring(message)) end
    local ok, failure = pcall(function()
        while true do
            local block, read_error = input:read(1024 * 1024)
            if read_error then error(read_error) end
            if not block then break end
            local written, write_error = output:write(block)
            if not written then error(write_error) end
        end
    end)
    input:close()
    local closed, close_error = output:close()
    if not ok then error(failure) end
    if not closed then error(close_error) end
end

local function runner(run)
    if not windows then return host end
    return {
        windows = true,
        exists = exists,
        execute = function(command)
            -- The documented control.execute Windows limitation permits only one quoted item.
            -- Write our command in the already reserved run folder and invoke that batch file.
            -- https://docs.darktable.org/lua/stable/lua.scripts.api.manual/dtutils.system/windows_command/
            local path = Bridge.join(run.inputs, "stack.cmd")
            local file, message = io.open(path, "wb")
            if not file then error("Could not write the CLI launcher: " .. tostring(message)) end
            local written, write_error = file:write("@echo off\r\nsetlocal DisableDelayedExpansion\r\n" .. command .. "\r\nexit /b %errorlevel%\r\n")
            local closed, close_error = file:close()
            if not written then error(write_error) end
            if not closed then error(close_error) end
            -- CALL keeps cmd.exe from stripping quotes around a leading executable path.
            return dt.control.execute("call " .. Bridge.quote(path, true))
        end,
    }
end

local function finalize(_, image_table, data)
    if data.failed or not data.run then return end
    local ok, message = pcall(function()
        local count = 0
        for _ in pairs(image_table) do count = count + 1 end
        if count ~= #data.images then error("The export is incomplete; no stack was started. Inputs: " .. data.run.inputs) end
        for index, image in ipairs(data.images) do
            local filename = image_table[image]
            if not filename then error("An expected frame was not exported; no stack was started.") end
            copy(filename, Bridge.join(data.run.inputs, string.format("%06d.tif", index)))
        end
        local output = Bridge.run(data.settings, data.run, runner(data.run))
        local imported = dt.database.import(output)
        if not imported then error("The result was saved but could not be imported: " .. output) end
        imported:group_with(data.images[1])
        dt.print(branding.name .. ": " .. output)
    end)
    if not ok then report(message) end
end

dt.register_storage(namespace, "Stack with " .. branding.name, nil, finalize, supported, initialize)

return {
    destroy = function() dt.destroy_storage(namespace) end,
    restart = function() dt.register_storage(namespace, "Stack with " .. branding.name, nil, finalize, supported, initialize) end,
}
