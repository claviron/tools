pub mod dns {
    include!(concat!(env!("OUT_DIR"), "/claviron.dns.rs"));
}

pub mod domains {
    include!(concat!(env!("OUT_DIR"), "/claviron.domains.rs"));
}

pub mod acme {
    include!(concat!(env!("OUT_DIR"), "/claviron.acme.rs"));
}

pub mod certificates {
    include!(concat!(env!("OUT_DIR"), "/claviron.certificates.rs"));
}

pub mod aliases {
    include!(concat!(env!("OUT_DIR"), "/claviron.aliases.rs"));
}

pub mod applications {
    include!(concat!(env!("OUT_DIR"), "/claviron.applications.rs"));
}

pub mod linked_applications {
    include!(concat!(env!("OUT_DIR"), "/claviron.linked_applications.rs"));
}

pub mod mtls {
    include!(concat!(env!("OUT_DIR"), "/claviron.mtls.rs"));
}

pub mod helloworld {
    include!(concat!(env!("OUT_DIR"), "/claviron.helloworld.rs"));
}

pub mod server {
    include!(concat!(env!("OUT_DIR"), "/claviron.server.rs"));
}

pub mod services {
    include!(concat!(env!("OUT_DIR"), "/claviron.services.rs"));
}

pub mod tasks {
    include!(concat!(env!("OUT_DIR"), "/claviron.tasks.rs"));
}

pub mod workers {
    include!(concat!(env!("OUT_DIR"), "/claviron.workers.rs"));
}
