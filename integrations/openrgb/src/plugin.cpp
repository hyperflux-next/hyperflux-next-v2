// SPDX-License-Identifier: GPL-2.0-only
//
// The OpenRGB plugin: shows devices paired with a HyperFlux V2 receiver as
// OpenRGB's own Razer controllers, and adds the HyperFlux Next V2 page with
// their batteries to the Information list.

#include "discovery.hpp"
#include "log.hpp"
#include "mirror.hpp"
#include "page_widget.hpp"

#include "OpenRGBPluginInterface.h"
#include "RGBController_Razer.h"
#include "RazerController.h"
#include "RazerDevices.h"
#include "ResourceManagerCallback.h"

#include <QCoreApplication>
#include <QObject>

#include <map>
#include <memory>
#include <mutex>
#include <utility>

static_assert(OPENRGB_PLUGIN_API_VERSION == 5, "built against an OpenRGB with another plugin API");

namespace hyperflux::openrgb
{
namespace
{

// OpenRGB's record for the route's device, if it has one of the same type.
const razer_device* openrgb_device(const Route& route)
{
    const device_type wanted = route.kind == DeviceKind::Mouse ? DEVICE_TYPE_MOUSE : DEVICE_TYPE_KEYBOARD;
    for(unsigned int i = 0; i < RAZER_NUM_DEVICES; ++i)
        if(device_list[i] != nullptr && device_list[i]->pid == route.product_id && device_list[i]->type == wanted)
            return device_list[i];
    return nullptr;
}

std::unique_ptr<RGBController> make_controller(RouteSession& session, const Route& route)
{
    const razer_device* device = openrgb_device(route);
    if(device == nullptr)
        return nullptr;
    // The location names the receiver's USB port and the lane, never the
    // receiver's connection. OpenRGB profiles still match after a replug.
    const std::string location = "hyperflux-next/native/" + route.receiver_id + "/" + route.device_id;
    auto* razer = new RazerController(session.hid(), nullptr, location.c_str(), route.product_id, device->name);
    return std::make_unique<RGBController_Razer>(razer);
}

std::string trimmed(const std::string& text)
{
    const auto first = text.find_first_not_of(" \t\r\n");
    return first == std::string::npos ? std::string() : text.substr(first, text.find_last_not_of(" \t\r\n") - first + 1);
}

// Serials of the Razer devices OpenRGB drives itself over HID, with the
// connection its controller name gives ("(Wired)", "(Wireless)").
std::map<std::string, std::string> direct_serials(OpenRGBPluginAPIInterface& api)
{
    std::map<std::string, std::string> result;
    for(RGBControllerInterface* controller : api.GetRGBControllers())
    {
        if(controller == nullptr || controller->GetVendor() != "Razer")
            continue;
        const std::string location = controller->GetLocation();
        if(location.size() <= 5 || !location.starts_with("HID: ") || location.starts_with("HID: hyperflux-next/native/"))
            continue;
        const std::string serial = trimmed(controller->GetSerial());
        if(serial.empty())
            continue;
        const std::string name = controller->GetName();
        const std::string connection = name.ends_with("(Wired)")      ? "Wired"
                                       : name.ends_with("(Wireless)") ? "Wireless"
                                                                      : "";
        // Two of OpenRGB's controllers for one serial that disagree say nothing.
        const auto [found, added] = result.try_emplace(serial, connection);
        if(!added && found->second != connection)
            found->second.clear();
    }
    return result;
}

const char* state_word(BridgeStatus::State state)
{
    switch(state)
    {
        case BridgeStatus::State::Ready: return "connected";
        case BridgeStatus::State::Connecting: return "connecting";
        case BridgeStatus::State::Recovering: return "reconnecting";
        case BridgeStatus::State::Failed: return "not answering";
    }
    return "";
}

} // namespace

class Plugin final : public QObject, public OpenRGBPluginInterface
{
    Q_OBJECT
    Q_PLUGIN_METADATA(IID OpenRGBPluginInterface_IID FILE "hyperflux-next-openrgb.json")
    Q_INTERFACES(OpenRGBPluginInterface)

public:
    ~Plugin() override { Unload(); }

    OpenRGBPluginInfo GetPluginInfo() override
    {
        OpenRGBPluginInfo info;
        // OpenRGB remembers whether a plugin is enabled by its name and
        // description together, which gives the V2 name a new entry there.
        info.Name = "HyperFlux Next V2";
        info.Description = "Razer devices paired with a HyperFlux V2 receiver, and their batteries";
        info.Version = HFX_VERSION;
        info.Commit = HFX_COMMIT;
        info.URL = HFX_PROJECT_URL;
        info.Location = OPENRGB_PLUGIN_LOCATION_INFORMATION;
        info.Label = "HyperFlux Next V2";
        info.ProtocolVersion = 0; // no OpenRGB SDK commands
        return info;
    }

    unsigned int GetPluginAPIVersion() override { return OPENRGB_PLUGIN_API_VERSION; }

    void Load(OpenRGBPluginAPIInterface* api) override
    {
        if(api == nullptr || api_ != nullptr)
            return;
        api_ = api;
        set_log_target(api);
        mirrors_ = std::make_unique<MirrorSet>(*api);
        page_->input = PageInput {};
        page_->input.loaded = true;
        reported_ = false;
        // Updates are applied on the GUI thread, where OpenRGB wants its
        // device list changed. Deleting the inbox drops any still queued.
        inbox_ = new QObject();
        inbox_->moveToThread(QCoreApplication::instance()->thread());
        QObject* inbox = inbox_;
        DiscoveryConfig config;
        config.make_controller = make_controller;
        discovery_ = std::make_unique<Discovery>(
            std::move(config),
            [this, inbox](std::shared_ptr<DiscoveryUpdate> update)
            { QMetaObject::invokeMethod(inbox, [this, update] { apply(*update); }, Qt::QueuedConnection); });
        emit changed();
    }

    QWidget* GetWidget() override
    {
        // The page reads shared state rather than the plugin: OpenRGB keeps
        // the widget around after it unloads the plugin.
        auto* widget = new PageWidget(
            [page = page_]
            {
                PageInput input = page->input;
                input.now_ms = boottime_ms();
                return make_page(input);
            });
        connect(this, &Plugin::changed, widget, &PageWidget::refresh);
        return widget;
    }

    QMenu* GetTrayMenu() override { return nullptr; }

    void Unload() override
    {
        if(api_ == nullptr)
            return;
        mirrors_->begin_unload();
        discovery_.reset(); // joined: nothing is posted after this
        QObject* inbox = nullptr;
        {
            std::lock_guard lock(inbox_mutex_);
            inbox = std::exchange(inbox_, nullptr);
        }
        delete inbox;
        for(auto& [key, route] : routes_)
            if(route.shown)
                mirrors_->remove(*route.controller->controller);
        routes_.clear();
        mirrors_.reset();
        page_->input.stopped = true;
        emit changed();
        set_log_target(nullptr);
        api_ = nullptr;
    }

    void OnProfileAboutToLoad() override {}
    void OnProfileLoad(nlohmann::json) override {}
    nlohmann::json OnProfileSave() override { return {}; }

    unsigned char* OnSDKCommand(unsigned int, unsigned char*, unsigned int* size) override
    {
        if(size != nullptr)
            *size = 0;
        return nullptr;
    }

    void ProfileManagerUpdated(unsigned int) override {}
    void SettingsManagerUpdated(unsigned int) override {}

    // Called from OpenRGB's detection thread.
    void ResourceManagerUpdated(unsigned int reason) override
    {
        if(reason != RESOURCEMANAGER_UPDATE_REASON_DETECTION_COMPLETE)
            return;
        std::lock_guard lock(inbox_mutex_);
        if(inbox_ != nullptr)
            QMetaObject::invokeMethod(inbox_, [this] { reconcile(); }, Qt::QueuedConnection);
    }

signals:
    void changed();

private:
    struct Shown
    {
        std::unique_ptr<RouteController> controller;
        bool shown = false;
    };

    struct PageSource
    {
        PageInput input;
    };

    void apply(DiscoveryUpdate& update)
    {
        if(!reported_ || update.status.state != page_->input.status.state)
            log_info("[HyperFlux Next V2] bridge %s%s%s", state_word(update.status.state),
                     update.status.error.empty() ? "" : ": ", update.status.error.c_str());
        reported_ = true;
        page_->input.status = update.status;
        if(update.snapshot)
            page_->input.snapshot = std::move(update.snapshot);
        for(const auto& key : update.removed)
        {
            const auto found = routes_.find(key);
            if(found == routes_.end())
                continue;
            if(found->second.shown)
                mirrors_->remove(*found->second.controller->controller);
            // Joins the controller's threads, then closes its session.
            routes_.erase(found);
        }
        for(auto& added : update.added)
        {
            const std::string key = added->key;
            routes_[key] = Shown {std::move(added)};
        }
        reconcile();
    }

    // A device OpenRGB already drives itself under the same serial (over a
    // cable, or through its own dongle) isn't shown a second time. Only the
    // serial counts: names and product ids differ between connections.
    void reconcile()
    {
        if(api_ == nullptr)
            return;
        const auto direct = direct_serials(*api_);
        page_->input.direct.clear();
        for(auto& [key, route] : routes_)
        {
            RGBController* controller = route.controller->controller.get();
            if(controller == nullptr)
                continue;
            const auto found = direct.find(trimmed(controller->GetSerial()));
            if(found != direct.end())
            {
                page_->input.direct[key] = found->second;
                if(route.shown)
                    mirrors_->remove(*controller);
                route.shown = false;
            }
            else if(!route.shown)
            {
                mirrors_->add(*controller);
                route.shown = true;
            }
        }
        emit changed();
    }

    OpenRGBPluginAPIInterface* api_ = nullptr;
    bool reported_ = false;
    std::shared_ptr<PageSource> page_ = std::make_shared<PageSource>();
    std::unique_ptr<MirrorSet> mirrors_;
    std::map<std::string, Shown> routes_;
    std::unique_ptr<Discovery> discovery_;
    std::mutex inbox_mutex_;
    QObject* inbox_ = nullptr;
};

} // namespace hyperflux::openrgb

#include "plugin.moc"
