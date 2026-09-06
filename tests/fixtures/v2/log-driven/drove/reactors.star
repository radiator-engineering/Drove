def reactor(name, agent, model, script, after = []):
    slug = name + "-reactor"
    return tab("{}: {} reactor".format(model, name), panes = [
        pane(slug,
             serve = ["bash", ".context/bin/run-reactor.sh", script],
             ready = output("watching"),
             after = after,
             on_start = ["append-event.sh", "spawn", "agent=" + agent, "model=" + model,
                         "role=" + slug, "runtime=headless"],
             on_stop = ["append-event.sh", "retire", "agent=" + agent, "disposition=stopped"]),
    ])
