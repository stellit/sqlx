use std::collections::HashMap;
use std::future::Future;
use std::str::FromStr;

use futures_core::future::BoxFuture;

use sqlx_rt::test_block_on;

use crate::connection::{ConnectOptions, Connection};
use crate::database::Database;
use crate::error::Error;
use crate::pool::{Pool, PoolConnection, PoolOptions};

mod fixtures;

use crate::executor::Executor;
use crate::migrate::Migrator;
pub use fixtures::FixtureSnapshot;

pub trait TestSupport: Database {
    /// Get parameters to construct a `Pool` suitable for testing.
    ///
    /// This `Pool` instance will behave somewhat specially:
    /// * all handles share a single global semaphore to avoid exceeding the max connections
    ///   for the database flavor.
    /// * each invocation results in a different temporary database.
    ///
    /// The passed `ConnectOptions` will be used to manage the test databases.
    /// The user credentials it contains must have the privilege to create and drop databases.
    fn test_context<'a>(
        master_opts: <Self::Connection as Connection>::Options,
        test_path: &'a str,
    ) -> BoxFuture<'a, Result<TestContext<Self>, Error>>;

    fn cleanup_test(db_name: String) -> BoxFuture<'static, Result<(), Error>>;

    /// Cleanup any test databases that are no longer in-use.
    fn cleanup_test_dbs<'a>(
        opts: <Self::Connection as Connection>::Options,
    ) -> BoxFuture<'a, Result<usize, Error>>;

    /// Take a snapshot of the current state of the database (data only).
    ///
    /// This snapshot can then be used to generate test fixtures.
    fn snapshot(conn: &mut Self::Connection)
        -> BoxFuture<'_, Result<FixtureSnapshot<Self>, Error>>;
}

pub struct TestFixture {
    pub path: &'static str,
    pub contents: &'static str,
}

pub struct TestArgs {
    test_path: &'static str,
    migrator: Option<Migrator>,
    fixtures: &'static [TestFixture],
}

pub trait TestFn<DB: Database> {
    type Output: TestTermination;

    fn run_test(self, args: TestArgs) -> Self::Output;
}

pub trait TestTermination {
    fn is_success(&self) -> bool;
}

pub struct TestContext<DB: Database> {
    pub pool_opts: PoolOptions<DB>,
    pub connect_opts: <DB::Connection as Connection>::Options,
    pub db_name: String,
}

impl<DB, F, Fut> TestFn<DB> for F
where
    F: FnOnce(Pool<DB>) -> Fut,
    DB: Database,
    Fut: Future,
    Fut::Output: TestTermination,
{
    type Output = Fut::Output;

    fn run_test(self, args: TestArgs) -> Self::Output {
        run_test(test_path, |pool_opts, connect_opts| async move {
            let pool = pool_opts
                .connect_with(connect_opts)
                .await
                .expect("failed to create pool");

            if let Some(migrator) = args.migrator {
                migrator
                    .run(&pool)
                    .await
                    .expect("failed to apply migrations");
            }

            for fixture in args.fixtures {
                pool.execute(fixture.contents)
                    .await
                    .unwrap_or_else(|| panic!("failed to apply fixture {:?}", fixture.path));
            }

            (self)(pool).await
        })
    }
}

impl<DB, F, Fut> TestFn<DB> for F
where
    DB: Database,
    F: FnOnce(PoolConnection<DB>) -> Fut,
    Fut: Future,
    Fut::Output: TestTermination,
{
    type Output = Fut::Output;

    fn run_test(self, args: TestArgs) -> Self::Output {
        TestFn::run_test(
            |pool: Pool<DB>| async move {
                let conn = pool.acquire().await.expect("failed to acquire connection");
                (self)(conn).await
            },
            args,
        )
    }
}

impl<'a, DB, F, Fut> TestFn<DB> for F
where
    DB: Database,
    F: FnOnce(&'a mut DB::Connection) -> Fut,
    Fut: Future + 'a,
    Fut::Output: TestTermination,
{
    type Output = Fut::Output;

    fn run_test(self, args: TestArgs) -> Self::Output {
        TestFn::run_test(
            |mut conn: PoolConnection<DB>| async move { (self)(&mut conn).await },
            args,
        )
    }
}

impl<DB, F, Fut> TestFn<DB> for F
where
    DB: Database,
    F: FnOnce(PoolOptions<DB>, <DB::Connection as Connection>::Options) -> Fut,
    Fut: Future,
    Fut::Output: TestTermination,
{
    type Output = Fut::Output;

    fn run_test(self, args: TestArgs) -> Self::Output {
        // We use the `Pool` impl to automatically migrate and apply fixtures.
        TestFn::run_test(
            |pool: Pool<DB>| {
                let pool_options = PoolOptions::clone(pool.options());
                let connect_options = pool.connect_options().clone();

                pool.close().await;

                (self)(pool_options, connect_options).await
            },
            args,
        )
    }
}

impl TestArgs {
    pub fn new(test_path: &'static str) -> Self {
        TestArgs {
            test_path,
            migrator: None,
            fixtures: &[],
        }
    }

    pub fn migrator(&mut self, migrator: Migrator) {
        self.migrator = Some(migrator);
    }

    pub fn fixtures(&mut self, fixtures: &'static [TestFixture]) {
        self.fixtures = fixtures;
    }
}

impl TestTermination for () {
    fn is_success(&self) -> bool {
        true
    }
}

impl TestTermination for ! {
    fn is_success(&self) -> bool {
        true
    }
}

impl<T, E> TestTermination for Result<T, E> {
    fn is_success(&self) -> bool {
        self.is_ok()
    }
}

fn run_test<DB, F, Fut>(test_path: &str, test_fn: F) -> Fut::Output
where
    DB: TestSupport,
    F: FnOnce(PoolOptions<DB>, <DB::Connection as Connection>::Options) -> Fut,
    Fut::Output: TestTermination,
{
    let url = dotenvy::var("DATABASE_URL").expect("DATABASE_URL must be set with `#[sqlx::test]`");

    let master_opts = <DB::Connection as Connection>::Options::from_str(&url)
        .expect("failed to parse DATABASE_URL");

    test_block_on(async move {
        let test_context = DB::test_context(master_opts, test_path)
            .await
            .expect("failed to connect to DATABASE_URL");

        let res = test_fn(test_context.pool_opts, test_context.connect_opts).await;

        if res.is_success() {
            if let Err(e) = DB::cleanup_test(test_context.db_name).await {}
        }
    })
}
