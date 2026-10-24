//! Server: which server, logging in, its users and its settings.

use super::*;

pub(super) fn bind(b: &Binder) {
    b.bind("Server", "find", Box::new(|app, _| app.server.find()));
    b.bind(
        "Server",
        "choose-found",
        Box::new(|app, args| app.server.choose_found(index(args, 0))),
    );
    b.bind(
        "Server",
        "choose-address",
        Box::new(|app, args| app.server.choose_address(&text_arg(args, 0))),
    );
    b.bind("Server", "log-in", Box::new(|app, _| app.server.log_in()));
    b.bind(
        "Server",
        "cancel-login",
        Box::new(|app, _| app.server.cancel_login()),
    );
    b.bind(
        "Server",
        "use-this-computer",
        Box::new(|app, _| app.use_this_computer()),
    );
    b.bind(
        "Server",
        "change-password",
        Box::new(|app, _| {
            let remote = app.library.remote();
            app.server.change_password(remote.as_deref());
        }),
    );
    b.bind("Server", "log-out", Box::new(|app, _| app.log_out()));
    b.bind(
        "Server",
        "verify-code",
        Box::new(|app, args| app.server.verify_code(&text_arg(args, 0))),
    );
    b.bind(
        "Server",
        "show-connection-code",
        Box::new(|app, _| {
            let remote = app.library.remote();
            app.server.show_connection_code(remote.as_deref());
        }),
    );
    b.bind(
        "Server",
        "invite-user",
        Box::new(|app, args| {
            let remote = app.library.remote();
            let admin = matches!(args.get(1), Some(Value::Bool(true)));
            app.server.invite_user(remote, &text_arg(args, 0), admin);
        }),
    );
    b.bind(
        "Server",
        "invite-again",
        Box::new(|app, args| {
            let remote = app.library.remote();
            app.server.invite_again(remote, &text_arg(args, 0));
        }),
    );
    b.bind(
        "Server",
        "remove-user",
        Box::new(|app, args| {
            let remote = app.library.remote();
            app.server.remove_user(remote, &text_arg(args, 0));
        }),
    );
    b.bind(
        "Server",
        "hide-invite-code",
        Box::new(|app, _| app.server.hide_invite_code()),
    );
    b.bind(
        "Server",
        "revoke",
        Box::new(|app, args| {
            let remote = app.library.remote();
            app.server.revoke(remote.as_deref(), &text_arg(args, 0));
        }),
    );
    b.bind(
        "Server",
        "set-network",
        Box::new(|app, args| {
            if let Some(Value::Bool(on)) = args.first() {
                app.library.set_network(*on);
            }
        }),
    );
    b.bind(
        "Server",
        "set-start-at-login",
        Box::new(|app, args| {
            if let Some(Value::Bool(on)) = args.first() {
                app.library.set_start_at_login(*on);
            }
        }),
    );
    b.bind(
        "Server",
        "check-update",
        Box::new(|app, _| {
            let remote = app.library.remote();
            app.server.check_update(remote);
        }),
    );
    b.bind(
        "Server",
        "install-update",
        Box::new(|app, _| {
            let remote = app.library.remote();
            app.server.install_update(remote);
        }),
    );
    b.bind(
        "Server",
        "set-server-name",
        Box::new(|app, args| {
            app.library.set_server_name(&text_arg(args, 0));
            let remote = app.library.remote();
            app.server.refresh(remote.as_deref());
        }),
    );
    b.bind(
        "Server",
        "browse-start",
        Box::new(|app, _| {
            let remote = app.library.remote();
            app.server.browse(remote.as_deref(), None);
        }),
    );
    b.bind(
        "Server",
        "browse-into",
        Box::new(|app, args| {
            let remote = app.library.remote();
            if let Some(folder) = app.server.browsed_into(index(args, 0)) {
                app.server.browse(remote.as_deref(), Some(&folder));
            }
        }),
    );
    b.bind(
        "Server",
        "browse-up",
        Box::new(|app, _| {
            let remote = app.library.remote();
            let up = app.server.browsed_up();
            app.server.browse(remote.as_deref(), up.as_deref());
        }),
    );
    b.bind(
        "Server",
        "browse-add",
        Box::new(|app, _| {
            if let Some(folder) = app.server.browsed() {
                app.add_draft_folder(folder);
            }
            app.server.stop_browsing();
        }),
    );
    b.bind(
        "Server",
        "browse-cancel",
        Box::new(|app, _| app.server.stop_browsing()),
    );
}
