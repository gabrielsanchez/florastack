local LrApplication = import "LrApplication"
local LrDialogs = import "LrDialogs"
local LrFileUtils = import "LrFileUtils"
local LrPathUtils = import "LrPathUtils"
local LrTasks = import "LrTasks"
local LrView = import "LrView"
local branding = require "Branding"
local Bridge = require "Bridge"
local Provider = {}

Provider.exportPresetFields = {
    {key = "executable", default = ""},
    {key = "preset", default = "builtin:insect-macro"},
    {key = "method", default = "pyramid"},
}
Provider.hideSections = {"exportLocation", "fileNaming", "fileSettings", "imageSettings", "outputSharpening", "metadata", "video", "watermarking"}
Provider.allowFileFormats = {"TIFF"}
Provider.allowColorSpaces = {"sRGB"}
Provider.canExportVideo = false

function Provider.updateExportSettings(settings)
    settings.LR_format = "TIFF"
    settings.LR_tiff_bitDepth = 16
    settings.LR_tiff_compressionMethod = "compressionMethod_ZIP"
    settings.LR_export_colorSpace = "sRGB"
    settings.LR_size_doConstrain = false
    settings.LR_outputSharpeningOn = false
    settings.LR_useWatermark = false
    settings.LR_minimizeEmbeddedMetadata = false
    settings.LR_removeLocationMetadata = false
    settings.LR_removeFaceMetadata = false
    settings.LR_reimportExportedPhoto = false
    settings.LR_renamingTokensOn = true
    settings.LR_tokens = "{{naming_sequenceNumber}}"
    settings.LR_initialSequenceNumber = 1
    settings.LR_collisionHandling = "rename"
    settings.LR_export_destinationType = "tempFolder"
end

function Provider.sectionsForTopOfDialog(factory, properties)
    return {{
        title = "Stack selected frames",
        synopsis = "16-bit TIFF, full size, sRGB, embedded metadata",
        factory:column {
            bind_to_object = properties,
            spacing = factory:control_spacing(),
            factory:static_text {title = "Absolute path to " .. branding.cli .. " (including .exe on Windows):"},
            factory:edit_field {value = LrView.bind "executable", width_in_chars = 60},
            factory:static_text {
                title = "Preset: builtin:insect-macro, builtin:product-shot, builtin:landscape,\n"
                    .. "builtin:microscope, or an absolute TOML path",
            },
            factory:edit_field {value = LrView.bind "preset", width_in_chars = 60},
            factory:popup_menu {value = LrView.bind "method", items = {
                {title = "Pyramid", value = "pyramid"},
                {title = "Depth map", value = "depthmap"},
                {title = "Weighted average", value = "average"},
            }},
            factory:static_text {title = "Inputs and logs are retained beside the result. See docs/integrations.md."},
        },
    }}
end

local function process(context, progress)
    local settings = context.propertyTable
    local host = {
        windows = WIN_ENV == true,
        exists = function(path) return not not LrFileUtils.exists(path) end,
        mkdir = function(path)
            return LrTasks.execute((WIN_ENV and "mkdir " or "mkdir -- ") .. Bridge.quote(path, WIN_ENV)) == 0
        end,
        execute = function(command)
            -- Adobe's SDK requires an extra outer pair of quotes on Windows.
            return LrTasks.execute(WIN_ENV and ('"' .. command .. '"') or command)
        end,
    }
    Bridge.check(settings, host.windows)
    if not LrPathUtils.isAbsolute(settings.executable) or LrFileUtils.exists(settings.executable) ~= "file" then
        error("Set the absolute path to the installed CLI executable in the export settings.")
    end
    local frames = {}
    local source
    for _, rendition in context:renditions {stopIfCanceled = true} do
        if progress:isCanceled() then return end
        local success, path = rendition:waitForRender()
        if not success then error("A frame could not be exported: " .. tostring(path)) end
        source = source or rendition.photo
        frames[#frames + 1] = path
    end
    if progress:isCanceled() then return end
    if #frames < 2 then error("Select at least two still photos in focus order.") end
    local original = source:getRawMetadata "path"
    local run = Bridge.reserve(LrPathUtils.parent(original), LrPathUtils.leafName(original), host)
    for index, path in ipairs(frames) do
        if progress:isCanceled() then return end
        local copied, message = LrFileUtils.copy(path, Bridge.join(run.inputs, string.format("%06d.tif", index)))
        if not copied then error("Could not retain an exported frame: " .. tostring(message) .. ". Inputs: " .. run.inputs) end
    end
    progress:setCaption("Stacking; the CLI must finish before this export can close.")
    local output = Bridge.run(settings, run, host)
    if progress:isCanceled() then return end
    local catalog = LrApplication.activeCatalog()
    local imported
    -- Only catalog import needs write access. Rendering and the child process run outside the lock.
    catalog:withWriteAccessDo("Import " .. branding.name .. " result", function()
        imported = catalog:addPhoto(output, source, "below")
    end)
    if not imported then error("The TIFF was saved but could not be imported into the catalog: " .. output) end
end

function Provider.processRenderedPhotos(_, context)
    local progress = context:configureProgress {title = "Stack with " .. branding.name}
    local success, message = LrTasks.pcall(function() process(context, progress) end)
    progress:done()
    if not success then LrDialogs.message(branding.name, tostring(message), "critical") end
end

return Provider
