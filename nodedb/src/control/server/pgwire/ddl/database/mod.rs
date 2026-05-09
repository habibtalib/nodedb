// SPDX-License-Identifier: BUSL-1.1

pub mod alter;
pub mod clone;
pub mod create;
pub mod drop;
pub mod materialize;
pub mod show;
pub mod show_lineage;
pub mod show_quota;
pub mod show_usage;
pub mod use_database;

pub use alter::handle_alter_database;
pub use clone::handle_clone_database;
pub use create::handle_create_database;
pub use drop::handle_drop_database;
pub use materialize::handle_alter_database_materialize;
pub use show::handle_show_databases;
pub use show_lineage::handle_show_database_lineage;
pub use show_quota::handle_show_database_quota;
pub use show_usage::handle_show_database_usage;
pub use use_database::handle_use_database;
