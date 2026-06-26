use crate::error::{Result, RmlError};
use crate::namespaces;
use crate::store::{QuadStore, InMemoryQuadStore};
use crate::term::{NamedNode, Term, TermRef};
use super::Access;
use std::io::{Cursor, Read};

use std::path::{Path, PathBuf};
use super::LocalFileAccess;
use super::remote::RemoteFileAccess;


/// Database type enumeration
///
/// Represents the different types of relational databases supported.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DatabaseType {
    /// MySQL database
    MySQL,
    /// PostgreSQL database
    PostgreSQL,
    /// SQLite database
    SQLite,
    /// Microsoft SQL Server
    MSSQL,
}

impl DatabaseType {
    /// Returns the JDBC driver name for compatibility with Java RML Mapper
    ///
    /// # Returns
    ///
    /// The JDBC driver class name as a string
    pub fn jdbc_driver_name(&self) -> &'static str {
        match self {
            DatabaseType::MySQL => "com.mysql.jdbc.Driver",
            DatabaseType::PostgreSQL => "org.postgresql.Driver",
            DatabaseType::SQLite => "org.sqlite.JDBC",
            DatabaseType::MSSQL => "com.microsoft.sqlserver.jdbc.SQLServerDriver",
        }
    }

    /// Returns the URL scheme for this database type
    ///
    /// # Returns
    ///
    /// The URL scheme (e.g., "mysql", "postgresql")
    pub fn url_scheme(&self) -> &'static str {
        match self {
            DatabaseType::MySQL => "mysql",
            DatabaseType::PostgreSQL => "postgresql",
            DatabaseType::SQLite => "sqlite",
            DatabaseType::MSSQL => "sqlserver",
        }
    }

    /// Detects database type from DSN/connection string
    ///
    /// # Arguments
    ///
    /// * `dsn` - The database connection string
    ///
    /// # Returns
    ///
    /// The detected database type, or None if not recognized
    pub fn from_dsn(dsn: &str) -> Option<Self> {
        if dsn.starts_with("mysql://") || dsn.contains("mysql") {
            Some(DatabaseType::MySQL)
        } else if dsn.starts_with("postgresql://") || dsn.starts_with("postgres://") || dsn.contains("postgres") {
            Some(DatabaseType::PostgreSQL)
        } else if dsn.starts_with("sqlite://") || dsn.contains("sqlite") {
            Some(DatabaseType::SQLite)
        } else if dsn.starts_with("sqlserver://") || dsn.contains("sqlserver") || dsn.contains("mssql") {
            Some(DatabaseType::MSSQL)
        } else {
            None
        }
    }
}

impl std::fmt::Display for DatabaseType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DatabaseType::MySQL => write!(f, "MySQL"),
            DatabaseType::PostgreSQL => write!(f, "PostgreSQL"),
            DatabaseType::SQLite => write!(f, "SQLite"),
            DatabaseType::MSSQL => write!(f, "MSSQL"),
        }
    }
}

/// Access to relational databases
///
/// Provides access to relational databases (MySQL, PostgreSQL, SQLite, MSSQL).
/// Query results are returned as CSV-like data.
///
/// # Examples
///
/// ```
/// use rml_mapper::access::{DatabaseAccess, DatabaseType};
///
/// let access = DatabaseAccess::new(
///     "postgresql://localhost/mydb".to_string(),
///     DatabaseType::PostgreSQL,
///     "user".to_string(),
///     "password".to_string(),
///     "SELECT * FROM users".to_string()
/// );
/// ```
#[derive(Debug, Clone)]
pub struct DatabaseAccess {
    /// Database connection string (DSN)
    dsn: String,
    /// Database type
    database_type: DatabaseType,
    /// Username for authentication
    username: String,
    /// Password for authentication
    password: String,
    /// SQL query to execute
    query: String,
}

impl DatabaseAccess {
    /// Creates a new database access
    ///
    /// # Arguments
    ///
    /// * `dsn` - Database connection string
    /// * `database_type` - Type of database
    /// * `username` - Username for authentication
    /// * `password` - Password for authentication
    /// * `query` - SQL query to execute
    pub fn new(
        dsn: String,
        database_type: DatabaseType,
        username: String,
        password: String,
        query: String,
    ) -> Self {
        Self {
            dsn,
            database_type,
            username,
            password,
            query,
        }
    }

    /// Returns the database type
    pub fn database_type(&self) -> DatabaseType {
        self.database_type
    }

    /// Returns the SQL query
    pub fn query(&self) -> &str {
        &self.query
    }

    /// Builds a connection string for sqlx from the DSN and credentials
    fn connection_string(&self) -> String {
        match self.database_type {
            DatabaseType::MySQL => {
                // Parse DSN to extract host and database
                let (host, database) = self.extract_host_and_database();
                if !self.username.is_empty() {
                    format!(
                        "mysql://{}:{}@{}/{}",
                        self.username, self.password, host, database
                    )
                } else {
                    format!("mysql://{}/{}", host, database)
                }
            }
            DatabaseType::PostgreSQL => {
                let (host, database) = self.extract_host_and_database();
                if !self.username.is_empty() {
                    format!(
                        "postgresql://{}:{}@{}/{}",
                        self.username, self.password, host, database
                    )
                } else {
                    format!("postgresql://{}/{}", host, database)
                }
            }
            DatabaseType::SQLite => {
                // SQLite uses file paths, extract from DSN
                if self.dsn.starts_with("sqlite://") {
                    // Remove one slash to get sqlite:/path format
                    self.dsn.replace("sqlite://", "sqlite:")
                } else if self.dsn.starts_with("sqlite:/") {
                    self.dsn.clone()
                } else if self.dsn.contains("sqlite:") {
                    // Handle JDBC-style: jdbc:sqlite:/path/to/db
                    let path = self.dsn.split("sqlite:").nth(1).unwrap_or("");
                    format!("sqlite:{}", path)
                } else {
                    // Assume it's a file path
                    format!("sqlite:{}", self.dsn)
                }
            }
            DatabaseType::MSSQL => {
                let (host, database) = self.extract_host_and_database();
                if !self.username.is_empty() {
                    format!(
                        "mssql://{}:{}@{}/{}",
                        self.username, self.password, host, database
                    )
                } else {
                    format!("mssql://{}/{}", host, database)
                }
            }
        }
    }

    /// Extracts host and database name from DSN
    fn extract_host_and_database(&self) -> (String, String) {
        // Handle various DSN formats:
        // - mysql://host/database
        // - jdbc:mysql://host/database
        // - host/database
        
        let dsn = &self.dsn;
        
        // Remove protocol prefix if present
        let without_protocol = dsn
            .split("://")
            .last()
            .unwrap_or(dsn);
        
        // Split by / to get host and database
        let parts: Vec<&str> = without_protocol.split('/').collect();
        
        let host = parts.first().unwrap_or(&"localhost").to_string();
        let database = parts.get(1).unwrap_or(&"").to_string();
        
        (host, database)
    }

    /// Executes the query asynchronously
    async fn execute_query(&self) -> Result<Box<dyn Read + Send>> {
        match self.database_type {
            DatabaseType::MySQL => self.query_mysql().await,
            DatabaseType::PostgreSQL => self.query_postgres().await,
            DatabaseType::SQLite => self.query_sqlite().await,
            DatabaseType::MSSQL => self.query_mssql().await,
        }
    }

    /// Executes a MySQL query
    async fn query_mysql(&self) -> Result<Box<dyn Read + Send>> {
        use sqlx::mysql::{MySqlPoolOptions, MySqlRow};

        let connection_string = self.connection_string();
        
        let pool = MySqlPoolOptions::new()
            .max_connections(1)
            .connect(&connection_string)
            .await
            .map_err(|e| RmlError::Database(format!("MySQL connection failed: {}", e)))?;

        let rows: Vec<MySqlRow> = sqlx::query(&self.query)
            .fetch_all(&pool)
            .await
            .map_err(|e| RmlError::Database(format!("MySQL query failed: {}", e)))?;

        // Convert to CSV
        let csv_data = self.rows_to_csv_mysql(&rows)?;
        Ok(Box::new(Cursor::new(csv_data)))
    }

    /// Executes a PostgreSQL query
    async fn query_postgres(&self) -> Result<Box<dyn Read + Send>> {
        use sqlx::postgres::{PgPoolOptions, PgRow};

        let connection_string = self.connection_string();
        
        let pool = PgPoolOptions::new()
            .max_connections(1)
            .connect(&connection_string)
            .await
            .map_err(|e| RmlError::Database(format!("PostgreSQL connection failed: {}", e)))?;

        let rows: Vec<PgRow> = sqlx::query(&self.query)
            .fetch_all(&pool)
            .await
            .map_err(|e| RmlError::Database(format!("PostgreSQL query failed: {}", e)))?;

        // Convert to CSV
        let csv_data = self.rows_to_csv_postgres(&rows)?;
        Ok(Box::new(Cursor::new(csv_data)))
    }

    /// Executes a SQLite query
    async fn query_sqlite(&self) -> Result<Box<dyn Read + Send>> {
        use sqlx::sqlite::{SqlitePoolOptions, SqliteRow};

        let connection_string = self.connection_string();
        
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect(&connection_string)
            .await
            .map_err(|e| RmlError::Database(format!("SQLite connection failed: {}", e)))?;

        let rows: Vec<SqliteRow> = sqlx::query(&self.query)
            .fetch_all(&pool)
            .await
            .map_err(|e| RmlError::Database(format!("SQLite query failed: {}", e)))?;

        // Convert to CSV
        let csv_data = self.rows_to_csv_sqlite(&rows)?;
        Ok(Box::new(Cursor::new(csv_data)))
    }

    /// Executes an MSSQL query
    async fn query_mssql(&self) -> Result<Box<dyn Read + Send>> {
        // Note: sqlx doesn't support MSSQL yet, so we return an error
        Err(RmlError::Database(
            "MSSQL is not yet supported by sqlx. Please use MySQL, PostgreSQL, or SQLite.".to_string()
        ))
    }

    /// Converts MySQL rows to CSV format
    fn rows_to_csv_mysql(&self, rows: &[sqlx::mysql::MySqlRow]) -> Result<Vec<u8>> {
        use sqlx::{Column, Row, TypeInfo};

        let mut wtr = csv::Writer::from_writer(vec![]);

        // Write headers from column names
        if let Some(first_row) = rows.first() {
            let columns: Vec<&str> = first_row.columns().iter().map(|c| c.name()).collect();
            wtr.write_record(&columns)
                .map_err(|e| RmlError::Database(format!("Failed to write CSV headers: {}", e)))?;
        }

        // Write data rows
        for row in rows {
            let mut record = Vec::new();
            for (i, column) in row.columns().iter().enumerate() {
                let value = self.extract_mysql_value(row, i, column.type_info().name());
                record.push(value);
            }
            wtr.write_record(&record)
                .map_err(|e| RmlError::Database(format!("Failed to write CSV record: {}", e)))?;
        }

        wtr.into_inner()
            .map_err(|e| RmlError::Database(format!("Failed to finalize CSV: {}", e)))
    }

    /// Converts PostgreSQL rows to CSV format
    fn rows_to_csv_postgres(&self, rows: &[sqlx::postgres::PgRow]) -> Result<Vec<u8>> {
        use sqlx::{Column, Row, TypeInfo};

        let mut wtr = csv::Writer::from_writer(vec![]);

        // Write headers from column names
        if let Some(first_row) = rows.first() {
            let columns: Vec<&str> = first_row.columns().iter().map(|c| c.name()).collect();
            wtr.write_record(&columns)
                .map_err(|e| RmlError::Database(format!("Failed to write CSV headers: {}", e)))?;
        }

        // Write data rows
        for row in rows {
            let mut record = Vec::new();
            for (i, column) in row.columns().iter().enumerate() {
                let value = self.extract_postgres_value(row, i, column.type_info().name());
                record.push(value);
            }
            wtr.write_record(&record)
                .map_err(|e| RmlError::Database(format!("Failed to write CSV record: {}", e)))?;
        }

        wtr.into_inner()
            .map_err(|e| RmlError::Database(format!("Failed to finalize CSV: {}", e)))
    }

    /// Converts SQLite rows to CSV format
    fn rows_to_csv_sqlite(&self, rows: &[sqlx::sqlite::SqliteRow]) -> Result<Vec<u8>> {
        use sqlx::{Column, Row, TypeInfo};

        let mut wtr = csv::Writer::from_writer(vec![]);

        // Write headers from column names
        if let Some(first_row) = rows.first() {
            let columns: Vec<&str> = first_row.columns().iter().map(|c| c.name()).collect();
            wtr.write_record(&columns)
                .map_err(|e| RmlError::Database(format!("Failed to write CSV headers: {}", e)))?;
        }

        // Write data rows
        for row in rows {
            let mut record = Vec::new();
            for (i, column) in row.columns().iter().enumerate() {
                let value = self.extract_sqlite_value(row, i, column.type_info().name());
                record.push(value);
            }
            wtr.write_record(&record)
                .map_err(|e| RmlError::Database(format!("Failed to write CSV record: {}", e)))?;
        }

        wtr.into_inner()
            .map_err(|e| RmlError::Database(format!("Failed to finalize CSV: {}", e)))
    }

    /// Extracts a value from a MySQL row, handling various types
    fn extract_mysql_value(&self, row: &sqlx::mysql::MySqlRow, index: usize, _type_name: &str) -> String {
        use sqlx::{Row, ValueRef};

        // Check if value is NULL
        if row.try_get_raw(index).map(|v| v.is_null()).unwrap_or(false) {
            return String::new();
        }

        // Try to extract as string first (most common case)
        if let Ok(val) = row.try_get::<String, _>(index) {
            return val;
        }

        // Try other common types
        if let Ok(val) = row.try_get::<i64, _>(index) {
            return val.to_string();
        }
        if let Ok(val) = row.try_get::<i32, _>(index) {
            return val.to_string();
        }
        if let Ok(val) = row.try_get::<f64, _>(index) {
            return val.to_string();
        }
        if let Ok(val) = row.try_get::<f32, _>(index) {
            return val.to_string();
        }
        if let Ok(val) = row.try_get::<bool, _>(index) {
            return val.to_string();
        }

        // Fallback to empty string
        String::new()
    }

    /// Extracts a value from a PostgreSQL row, handling various types
    fn extract_postgres_value(&self, row: &sqlx::postgres::PgRow, index: usize, _type_name: &str) -> String {
        use sqlx::{Row, ValueRef};

        // Check if value is NULL
        if row.try_get_raw(index).map(|v| v.is_null()).unwrap_or(false) {
            return String::new();
        }

        // Try to extract as string first (most common case)
        if let Ok(val) = row.try_get::<String, _>(index) {
            return val;
        }

        // Try other common types
        if let Ok(val) = row.try_get::<i64, _>(index) {
            return val.to_string();
        }
        if let Ok(val) = row.try_get::<i32, _>(index) {
            return val.to_string();
        }
        if let Ok(val) = row.try_get::<i16, _>(index) {
            return val.to_string();
        }
        if let Ok(val) = row.try_get::<f64, _>(index) {
            return val.to_string();
        }
        if let Ok(val) = row.try_get::<f32, _>(index) {
            return val.to_string();
        }
        if let Ok(val) = row.try_get::<bool, _>(index) {
            return val.to_string();
        }

        // Fallback to empty string
        String::new()
    }

    /// Extracts a value from a SQLite row, handling various types
    fn extract_sqlite_value(&self, row: &sqlx::sqlite::SqliteRow, index: usize, _type_name: &str) -> String {
        use sqlx::{Row, ValueRef};

        // Check if value is NULL
        if row.try_get_raw(index).map(|v| v.is_null()).unwrap_or(false) {
            return String::new();
        }

        // Try to extract as string first (most common case)
        if let Ok(val) = row.try_get::<String, _>(index) {
            return val;
        }

        // Try other common types
        if let Ok(val) = row.try_get::<i64, _>(index) {
            return val.to_string();
        }
        if let Ok(val) = row.try_get::<i32, _>(index) {
            return val.to_string();
        }
        if let Ok(val) = row.try_get::<f64, _>(index) {
            return val.to_string();
        }
        if let Ok(val) = row.try_get::<bool, _>(index) {
            return val.to_string();
        }

        // Try to get as bytes and convert to string
        if let Ok(val) = row.try_get::<Vec<u8>, _>(index) {
            return String::from_utf8_lossy(&val).to_string();
        }

        // Fallback to empty string
        String::new()
    }
}

impl Access for DatabaseAccess {
    fn get_reader(&self) -> Result<Box<dyn Read + Send>> {
        // Create a Tokio runtime to execute the async query
        let rt = tokio::runtime::Runtime::new()
            .map_err(|e| RmlError::Database(format!("Failed to create async runtime: {}", e)))?;

        // Execute the query synchronously using the runtime
        rt.block_on(async {
            self.execute_query().await
        })
    }

    fn content_type(&self) -> Option<&str> {
        // Database results are returned as CSV
        Some("text/csv")
    }

    fn cache_key(&self) -> String {
        format!(
            "db://{}@{}/{}",
            self.username,
            self.dsn,
            // Use a hash of the query for the cache key
            self.query.chars().fold(0u64, |acc, c| acc.wrapping_mul(31).wrapping_add(c as u64))
        )
    }
}

/// SPARQL result format enumeration
///
/// Represents the different result formats supported by SPARQL endpoints.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SparqlResultFormat {
    /// SPARQL Results XML Format
    XML,
    /// SPARQL Results JSON Format
    JSON,
    /// SPARQL Results CSV Format
    CSV,
}

impl SparqlResultFormat {
    /// Returns the content type for this format
    ///
    /// # Returns
    ///
    /// The MIME type for this SPARQL result format
    pub fn content_type(&self) -> &'static str {
        match self {
            SparqlResultFormat::XML => "application/sparql-results+xml",
            SparqlResultFormat::JSON => "application/sparql-results+json",
            SparqlResultFormat::CSV => "text/csv",
        }
    }

    /// Returns the Accept header value for this format
    ///
    /// # Returns
    ///
    /// The Accept header value to request this format
    pub fn accept_header(&self) -> &'static str {
        self.content_type()
    }
}

impl std::fmt::Display for SparqlResultFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SparqlResultFormat::XML => write!(f, "XML"),
            SparqlResultFormat::JSON => write!(f, "JSON"),
            SparqlResultFormat::CSV => write!(f, "CSV"),
        }
    }
}

/// Access to SPARQL endpoints
///
/// Provides access to SPARQL endpoints for querying RDF data.
///
/// # Examples
///
/// ```
/// use rml_mapper::access::{SparqlAccess, SparqlResultFormat};
///
/// let access = SparqlAccess::new(
///     "https://dbpedia.org/sparql".to_string(),
///     "SELECT * WHERE { ?s ?p ?o } LIMIT 10".to_string(),
///     SparqlResultFormat::JSON
/// );
/// ```
#[derive(Debug, Clone)]
pub struct SparqlAccess {
    /// SPARQL endpoint URL
    endpoint_url: String,
    /// SPARQL query to execute
    query: String,
    /// Result format
    result_format: SparqlResultFormat,
}

impl SparqlAccess {
    /// Creates a new SPARQL access
    ///
    /// # Arguments
    ///
    /// * `endpoint_url` - URL of the SPARQL endpoint
    /// * `query` - SPARQL query to execute
    /// * `result_format` - Desired result format
    pub fn new(endpoint_url: String, query: String, result_format: SparqlResultFormat) -> Self {
        Self {
            endpoint_url,
            query,
            result_format,
        }
    }

    /// Returns the SPARQL query
    pub fn query(&self) -> &str {
        &self.query
    }

    /// Returns the result format
    pub fn result_format(&self) -> SparqlResultFormat {
        self.result_format
    }
}

impl Access for SparqlAccess {
    fn get_reader(&self) -> Result<Box<dyn Read + Send>> {
        let client = reqwest::blocking::Client::new();

        // URL encode the query
        let encoded_query = urlencoding::encode(&self.query);

        let response = client
            .post(&self.endpoint_url)
            .header("Accept", self.result_format.accept_header())
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(format!("query={}", encoded_query))
            .send()
            .map_err(|e| {
                RmlError::Http(format!(
                    "Failed to query SPARQL endpoint '{}': {}",
                    self.endpoint_url, e
                ))
            })?;

        if !response.status().is_success() {
            return Err(RmlError::Http(format!(
                "SPARQL query failed with status {}: {}",
                response.status(),
                self.endpoint_url
            )));
        }

        let bytes = response.bytes().map_err(|e| {
            RmlError::Http(format!(
                "Failed to read SPARQL response from '{}': {}",
                self.endpoint_url, e
            ))
        })?;

        Ok(Box::new(Cursor::new(bytes.to_vec())))
    }

    fn content_type(&self) -> Option<&str> {
        Some(self.result_format.content_type())
    }

    fn cache_key(&self) -> String {
        format!(
            "sparql://{}?query={}",
            self.endpoint_url,
            self.query.chars().fold(0u64, |acc, c| acc.wrapping_mul(31).wrapping_add(c as u64))
        )
    }
}

/// Factory for creating Access instances from RML logical sources
///
/// This factory parses RML logical source descriptions from a QuadStore
/// and creates the appropriate Access implementation.
///
/// # Examples
///
/// ```no_run
/// use rml_mapper::access::AccessFactory;
/// use rml_mapper::store::InMemoryQuadStore;
/// use rml_mapper::term::NamedNode;
///
/// let store = InMemoryQuadStore::new();
/// let logical_source = NamedNode::new("http://example.org/source1").unwrap();
/// let factory = AccessFactory::new();
/// // let access = factory.create_access(&store, &logical_source.into()).unwrap();
/// ```
pub struct AccessFactory {
    /// Base path for resolving relative file paths
    base_path: Option<PathBuf>,
}

impl AccessFactory {
    /// Creates a new access factory
    pub fn new() -> Self {
        Self { base_path: None }
    }

    /// Creates a new access factory with a base path
    ///
    /// # Arguments
    ///
    /// * `base_path` - Base path for resolving relative file paths
    pub fn with_base_path(base_path: PathBuf) -> Self {
        Self {
            base_path: Some(base_path),
        }
    }

    /// Creates an Access instance from an RML logical source
    ///
    /// # Arguments
    ///
    /// * `store` - QuadStore containing the RML mapping
    /// * `logical_source` - Term identifying the logical source
    ///
    /// # Returns
    ///
    /// An Access implementation appropriate for the logical source type
    pub fn create_access(
        &self,
        store: &InMemoryQuadStore,
        logical_source: &TermRef,
    ) -> Result<Box<dyn Access>> {
        // Get the source property (rml:source or rml:source)
        let source_pred = NamedNode::new(format!("{}source", namespaces::RML))
            .map_err(|e| RmlError::Parse(format!("Invalid RML namespace: {}", e)))?;

        let source_quads = store.get_quads(Some(logical_source), Some(&source_pred), None, None)?;

        if source_quads.is_empty() {
            return Err(RmlError::Access(
                "No rml:source property found for logical source".to_string(),
            ));
        }

        let source_value = source_quads[0].object();

        // Check if it's a file path or URL
        let source_str = source_value.value();

        if source_str.starts_with("http://") || source_str.starts_with("https://") {
            // Remote file access
            let content_type = self.detect_content_type(store, logical_source)?;
            Ok(Box::new(RemoteFileAccess::new(
                source_str.to_string(),
                content_type,
            )))
        } else if self.is_database_source(store, logical_source)? {
            // Database access
            self.create_database_access(store, logical_source)
        } else if self.is_sparql_source(store, logical_source)? {
            // SPARQL endpoint access
            self.create_sparql_access(store, logical_source)
        } else {
            // Local file access
            let content_type = self.detect_content_type(store, logical_source)?;
            Ok(Box::new(LocalFileAccess::new(
                PathBuf::from(source_str),
                self.base_path.clone(),
                content_type,
            )))
        }
    }

    /// Checks if the logical source is a database source
    fn is_database_source(&self, store: &InMemoryQuadStore, logical_source: &TermRef) -> Result<bool> {
        // Check for d2rq:jdbcDSN or rr:tableName
        let jdbc_dsn_pred = NamedNode::new(format!("{}jdbcDSN", namespaces::D2RQ))
            .map_err(|e| RmlError::Parse(format!("Invalid D2RQ namespace: {}", e)))?;
        let table_name_pred = NamedNode::new(format!("{}tableName", namespaces::RR))
            .map_err(|e| RmlError::Parse(format!("Invalid R2RML namespace: {}", e)))?;

        Ok(store
            .contains(Some(logical_source), Some(&jdbc_dsn_pred), None, None)?
            || store.contains(Some(logical_source), Some(&table_name_pred), None, None)?)
    }

    /// Checks if the logical source is a SPARQL source
    fn is_sparql_source(&self, store: &InMemoryQuadStore, logical_source: &TermRef) -> Result<bool> {
        // Check for sd:endpoint
        let endpoint_pred = NamedNode::new(format!("{}endpoint", namespaces::SD))
            .map_err(|e| RmlError::Parse(format!("Invalid SD namespace: {}", e)))?;

        store.contains(Some(logical_source), Some(&endpoint_pred), None, None)
    }

    /// Creates a database access from RML description
    fn create_database_access(
        &self,
        store: &InMemoryQuadStore,
        logical_source: &TermRef,
    ) -> Result<Box<dyn Access>> {
        // Get DSN
        let jdbc_dsn_pred = NamedNode::new(format!("{}jdbcDSN", namespaces::D2RQ))
            .map_err(|e| RmlError::Parse(format!("Invalid D2RQ namespace: {}", e)))?;
        let dsn_quads = store.get_quads(Some(logical_source), Some(&jdbc_dsn_pred), None, None)?;

        if dsn_quads.is_empty() {
            return Err(RmlError::Access(
                "No d2rq:jdbcDSN property found for database source".to_string(),
            ));
        }

        let dsn = dsn_quads[0].object().value().to_string();
        let database_type = DatabaseType::from_dsn(&dsn).ok_or_else(|| {
            RmlError::Access(format!("Could not determine database type from DSN: {}", dsn))
        })?;

        // Get username and password (optional)
        let username_pred = NamedNode::new(format!("{}username", namespaces::D2RQ))
            .map_err(|e| RmlError::Parse(format!("Invalid D2RQ namespace: {}", e)))?;
        let password_pred = NamedNode::new(format!("{}password", namespaces::D2RQ))
            .map_err(|e| RmlError::Parse(format!("Invalid D2RQ namespace: {}", e)))?;

        let username = store
            .get_quad(Some(logical_source), Some(&username_pred), None, None)?
            .map(|q| q.object().value().to_string())
            .unwrap_or_default();

        let password = store
            .get_quad(Some(logical_source), Some(&password_pred), None, None)?
            .map(|q| q.object().value().to_string())
            .unwrap_or_default();

        // Get query (rr:sqlQuery or rr:tableName)
        let sql_query_pred = NamedNode::new(format!("{}sqlQuery", namespaces::RR))
            .map_err(|e| RmlError::Parse(format!("Invalid R2RML namespace: {}", e)))?;
        let table_name_pred = NamedNode::new(format!("{}tableName", namespaces::RR))
            .map_err(|e| RmlError::Parse(format!("Invalid R2RML namespace: {}", e)))?;

        let query = if let Some(quad) =
            store.get_quad(Some(logical_source), Some(&sql_query_pred), None, None)?
        {
            quad.object().value().to_string()
        } else if let Some(quad) =
            store.get_quad(Some(logical_source), Some(&table_name_pred), None, None)?
        {
            format!("SELECT * FROM {}", quad.object().value())
        } else {
            return Err(RmlError::Access(
                "No rr:sqlQuery or rr:tableName property found for database source".to_string(),
            ));
        };

        Ok(Box::new(DatabaseAccess::new(
            dsn,
            database_type,
            username,
            password,
            query,
        )))
    }

    /// Creates a SPARQL access from RML description
    fn create_sparql_access(
        &self,
        store: &InMemoryQuadStore,
        logical_source: &TermRef,
    ) -> Result<Box<dyn Access>> {
        // Get endpoint URL
        let endpoint_pred = NamedNode::new(format!("{}endpoint", namespaces::SD))
            .map_err(|e| RmlError::Parse(format!("Invalid SD namespace: {}", e)))?;
        let endpoint_quads =
            store.get_quads(Some(logical_source), Some(&endpoint_pred), None, None)?;

        if endpoint_quads.is_empty() {
            return Err(RmlError::Access(
                "No sd:endpoint property found for SPARQL source".to_string(),
            ));
        }

        let endpoint_url = endpoint_quads[0].object().value().to_string();

        // Get query
        let query_pred = NamedNode::new(format!("{}query", namespaces::SD))
            .map_err(|e| RmlError::Parse(format!("Invalid SD namespace: {}", e)))?;
        let query_quads = store.get_quads(Some(logical_source), Some(&query_pred), None, None)?;

        if query_quads.is_empty() {
            return Err(RmlError::Access(
                "No sd:query property found for SPARQL source".to_string(),
            ));
        }

        let query = query_quads[0].object().value().to_string();

        // Get result format (default to JSON)
        let result_format = SparqlResultFormat::JSON;

        Ok(Box::new(SparqlAccess::new(
            endpoint_url,
            query,
            result_format,
        )))
    }

    /// Detects content type from RML description or file extension
    fn detect_content_type(
        &self,
        store: &InMemoryQuadStore,
        logical_source: &TermRef,
    ) -> Result<Option<String>> {
        // Check for explicit content type in RML (dcat:mediaType)
        let media_type_pred = NamedNode::new(format!("{}mediaType", namespaces::DCAT))
            .map_err(|e| RmlError::Parse(format!("Invalid DCAT namespace: {}", e)))?;

        if let Some(quad) =
            store.get_quad(Some(logical_source), Some(&media_type_pred), None, None)?
        {
            return Ok(Some(quad.object().value().to_string()));
        }

        // Try to detect from source path/URL
        let source_pred = NamedNode::new(format!("{}source", namespaces::RML))
            .map_err(|e| RmlError::Parse(format!("Invalid RML namespace: {}", e)))?;

        if let Some(quad) = store.get_quad(Some(logical_source), Some(&source_pred), None, None)? {
            let source_str = quad.object().value();
            if source_str.starts_with("http://") || source_str.starts_with("https://") {
                return Ok(RemoteFileAccess::detect_content_type_from_url(source_str));
            } else {
                return Ok(LocalFileAccess::detect_content_type(Path::new(source_str)));
            }
        }

        Ok(None)
    }
}

impl Default for AccessFactory {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use tempfile::NamedTempFile;
    use std::io::Write;

    #[test]
    fn test_local_file_access() {
        // Create a temporary file
        let mut temp_file = NamedTempFile::new().unwrap();
        writeln!(temp_file, "test data").unwrap();
        let path = temp_file.path().to_path_buf();

        let access = LocalFileAccess::new(path.clone(), None, Some("text/plain".to_string()));

        assert_eq!(access.content_type(), Some("text/plain"));
        assert!(access.cache_key().contains(&path.display().to_string()));

        let mut reader = access.get_reader().unwrap();
        let mut content = String::new();
        reader.read_to_string(&mut content).unwrap();
        assert!(content.contains("test data"));
    }

    #[test]
    fn test_local_file_access_relative_path() {
        let mut temp_file = NamedTempFile::new().unwrap();
        writeln!(temp_file, "test data").unwrap();
        let path = temp_file.path();
        let file_name = path.file_name().unwrap();
        let base_path = path.parent().unwrap().to_path_buf();

        let access = LocalFileAccess::new(
            PathBuf::from(file_name),
            Some(base_path),
            None,
        );

        let mut reader = access.get_reader().unwrap();
        let mut content = String::new();
        reader.read_to_string(&mut content).unwrap();
        assert!(content.contains("test data"));
    }

    #[test]
    fn test_local_file_access_nonexistent() {
        let access = LocalFileAccess::new(
            PathBuf::from("/nonexistent/file.txt"),
            None,
            None,
        );

        assert!(access.get_reader().is_err());
    }

    #[test]
    fn test_detect_content_type() {
        assert_eq!(
            LocalFileAccess::detect_content_type(Path::new("file.csv")),
            Some("text/csv".to_string())
        );
        assert_eq!(
            LocalFileAccess::detect_content_type(Path::new("file.json")),
            Some("application/json".to_string())
        );
        assert_eq!(
            LocalFileAccess::detect_content_type(Path::new("file.xml")),
            Some("application/xml".to_string())
        );
        assert_eq!(
            LocalFileAccess::detect_content_type(Path::new("file.ttl")),
            Some("text/turtle".to_string())
        );
        assert_eq!(
            LocalFileAccess::detect_content_type(Path::new("file.unknown")),
            None
        );
    }

    #[test]
    fn test_database_type_from_dsn() {
        assert_eq!(
            DatabaseType::from_dsn("mysql://localhost/mydb"),
            Some(DatabaseType::MySQL)
        );
        assert_eq!(
            DatabaseType::from_dsn("postgresql://localhost/mydb"),
            Some(DatabaseType::PostgreSQL)
        );
        assert_eq!(
            DatabaseType::from_dsn("postgres://localhost/mydb"),
            Some(DatabaseType::PostgreSQL)
        );
        assert_eq!(
            DatabaseType::from_dsn("sqlite:///path/to/db.sqlite"),
            Some(DatabaseType::SQLite)
        );
        assert_eq!(
            DatabaseType::from_dsn("sqlserver://localhost/mydb"),
            Some(DatabaseType::MSSQL)
        );
        assert_eq!(DatabaseType::from_dsn("unknown://localhost/mydb"), None);
    }

    #[test]
    fn test_database_type_jdbc_driver() {
        assert_eq!(
            DatabaseType::MySQL.jdbc_driver_name(),
            "com.mysql.jdbc.Driver"
        );
        assert_eq!(
            DatabaseType::PostgreSQL.jdbc_driver_name(),
            "org.postgresql.Driver"
        );
        assert_eq!(DatabaseType::SQLite.jdbc_driver_name(), "org.sqlite.JDBC");
        assert_eq!(
            DatabaseType::MSSQL.jdbc_driver_name(),
            "com.microsoft.sqlserver.jdbc.SQLServerDriver"
        );
    }

    #[test]
    fn test_database_type_url_scheme() {
        assert_eq!(DatabaseType::MySQL.url_scheme(), "mysql");
        assert_eq!(DatabaseType::PostgreSQL.url_scheme(), "postgresql");
        assert_eq!(DatabaseType::SQLite.url_scheme(), "sqlite");
        assert_eq!(DatabaseType::MSSQL.url_scheme(), "sqlserver");
    }

    #[test]
    fn test_database_access_creation() {
        let access = DatabaseAccess::new(
            "postgresql://localhost/mydb".to_string(),
            DatabaseType::PostgreSQL,
            "user".to_string(),
            "password".to_string(),
            "SELECT * FROM users".to_string(),
        );

        assert_eq!(access.database_type(), DatabaseType::PostgreSQL);
        assert_eq!(access.query(), "SELECT * FROM users");
        assert_eq!(access.content_type(), Some("text/csv"));
        assert!(access.cache_key().contains("user"));
        assert!(access.cache_key().contains("postgresql://localhost/mydb"));
    }

    #[test]
    fn test_database_access_connection_failure() {
        let access = DatabaseAccess::new(
            "postgresql://nonexistent-host-12345/mydb".to_string(),
            DatabaseType::PostgreSQL,
            "user".to_string(),
            "password".to_string(),
            "SELECT * FROM users".to_string(),
        );

        // Should return error since connection will fail
        assert!(access.get_reader().is_err());
    }

    #[test]
    fn test_sparql_result_format_content_type() {
        assert_eq!(
            SparqlResultFormat::XML.content_type(),
            "application/sparql-results+xml"
        );
        assert_eq!(
            SparqlResultFormat::JSON.content_type(),
            "application/sparql-results+json"
        );
        assert_eq!(SparqlResultFormat::CSV.content_type(), "text/csv");
    }

    #[test]
    fn test_sparql_access_creation() {
        let access = SparqlAccess::new(
            "https://dbpedia.org/sparql".to_string(),
            "SELECT * WHERE { ?s ?p ?o } LIMIT 10".to_string(),
            SparqlResultFormat::JSON,
        );

        assert_eq!(access.query(), "SELECT * WHERE { ?s ?p ?o } LIMIT 10");
        assert_eq!(access.result_format(), SparqlResultFormat::JSON);
        assert_eq!(
            access.content_type(),
            Some("application/sparql-results+json")
        );
        assert!(access.cache_key().contains("dbpedia.org"));
    }

    #[test]
    fn test_remote_file_access_creation() {
        let access = RemoteFileAccess::new(
            "https://example.org/data.csv".to_string(),
            Some("text/csv".to_string()),
        );

        assert_eq!(access.content_type(), Some("text/csv"));
        assert_eq!(access.cache_key(), "https://example.org/data.csv");
    }

    #[test]
    fn test_remote_file_detect_content_type() {
        assert_eq!(
            RemoteFileAccess::detect_content_type_from_url("https://example.org/data.csv"),
            Some("text/csv".to_string())
        );
        assert_eq!(
            RemoteFileAccess::detect_content_type_from_url("https://example.org/data.json?param=value"),
            Some("application/json".to_string())
        );
        assert_eq!(
            RemoteFileAccess::detect_content_type_from_url("https://example.org/data.xml"),
            Some("application/xml".to_string())
        );
    }

    #[test]
    fn test_access_factory_creation() {
        let factory = AccessFactory::new();
        assert!(factory.base_path.is_none());

        let factory = AccessFactory::with_base_path(PathBuf::from("/base/path"));
        assert_eq!(factory.base_path, Some(PathBuf::from("/base/path")));
    }

    #[test]
    fn test_access_factory_default() {
        let factory = AccessFactory::default();
        assert!(factory.base_path.is_none());
    }

    // Integration tests with actual HTTP requests would go here
    // but are commented out to avoid network dependencies in tests

    /*
    #[test]
    fn test_remote_file_access_real_url() {
        let access = RemoteFileAccess::new(
            "https://httpbin.org/robots.txt".to_string(),
            Some("text/plain".to_string()),
        );

        let mut reader = access.get_reader().unwrap();
        let mut content = String::new();
        reader.read_to_string(&mut content).unwrap();
        assert!(!content.is_empty());
    }
    */

    #[test]
    fn test_access_factory_with_local_file() {
        use crate::store::InMemoryQuadStore;
        use crate::term::Literal;

        let mut store = InMemoryQuadStore::new();
        
        // Create a temporary file
        let mut temp_file = NamedTempFile::new().unwrap();
        writeln!(temp_file, "test,data").unwrap();
        let path = temp_file.path().to_str().unwrap();

        // Add RML triples for a local file source
        let source_node = NamedNode::new("http://example.org/source1").unwrap();
        let source_pred = NamedNode::new(&format!("{}source", namespaces::RML)).unwrap();
        
        store.add_quad(
            source_node.clone().into(),
            source_pred,
            Literal::new(path).into(),
            None,
        ).unwrap();

        let factory = AccessFactory::new();
        let access = factory.create_access(&store, &source_node.into()).unwrap();

        assert!(access.cache_key().contains(path));
        
        let mut reader = access.get_reader().unwrap();
        let mut content = String::new();
        reader.read_to_string(&mut content).unwrap();
        assert!(content.contains("test,data"));
    }

    #[test]
    fn test_access_factory_with_remote_file() {
        use crate::store::InMemoryQuadStore;
        use crate::term::Literal;

        let mut store = InMemoryQuadStore::new();

        // Add RML triples for a remote file source
        let source_node = NamedNode::new("http://example.org/source1").unwrap();
        let source_pred = NamedNode::new(&format!("{}source", namespaces::RML)).unwrap();
        
        store.add_quad(
            source_node.clone().into(),
            source_pred,
            Literal::new("https://example.org/data.csv").into(),
            None,
        ).unwrap();

        let factory = AccessFactory::new();
        let access = factory.create_access(&store, &source_node.into()).unwrap();

        assert_eq!(access.cache_key(), "https://example.org/data.csv");
        assert_eq!(access.content_type(), Some("text/csv"));
    }

    #[test]
    fn test_access_factory_with_content_type() {
        use crate::store::InMemoryQuadStore;
        use crate::term::Literal;

        let mut store = InMemoryQuadStore::new();
        
        let mut temp_file = NamedTempFile::new().unwrap();
        writeln!(temp_file, "test data").unwrap();
        let path = temp_file.path().to_str().unwrap();

        // Add RML triples with explicit content type
        let source_node = NamedNode::new("http://example.org/source1").unwrap();
        let source_pred = NamedNode::new(&format!("{}source", namespaces::RML)).unwrap();
        let media_type_pred = NamedNode::new(&format!("{}mediaType", namespaces::DCAT)).unwrap();
        
        store.add_quad(
            source_node.clone().into(),
            source_pred,
            Literal::new(path).into(),
            None,
        ).unwrap();

        store.add_quad(
            source_node.clone().into(),
            media_type_pred,
            Literal::new("application/custom").into(),
            None,
        ).unwrap();

        let factory = AccessFactory::new();
        let access = factory.create_access(&store, &source_node.into()).unwrap();

        assert_eq!(access.content_type(), Some("application/custom"));
    }

    #[test]
    fn test_database_type_display() {
        assert_eq!(format!("{}", DatabaseType::MySQL), "MySQL");
        assert_eq!(format!("{}", DatabaseType::PostgreSQL), "PostgreSQL");
        assert_eq!(format!("{}", DatabaseType::SQLite), "SQLite");
        assert_eq!(format!("{}", DatabaseType::MSSQL), "MSSQL");
    }

    #[test]
    fn test_sparql_result_format_display() {
        assert_eq!(format!("{}", SparqlResultFormat::XML), "XML");
        assert_eq!(format!("{}", SparqlResultFormat::JSON), "JSON");
        assert_eq!(format!("{}", SparqlResultFormat::CSV), "CSV");
    }

    #[test]
    fn test_access_trait_object() {
        let access: Box<dyn Access> = Box::new(LocalFileAccess::new(
            PathBuf::from("test.csv"),
            None,
            Some("text/csv".to_string()),
        ));

        assert_eq!(access.content_type(), Some("text/csv"));
        assert!(access.cache_key().contains("test.csv"));
    }

    // Database integration tests
    // These tests are ignored by default and require actual database instances to run
    
    #[test]
    #[ignore]
    fn test_sqlite_database_access() {
        use std::io::Read;
        
        // Create a temporary SQLite database
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("test.db");
        let db_path_str = db_path.to_str().unwrap();
        
        // Create and populate the database
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            use sqlx::sqlite::SqlitePoolOptions;
            use sqlx::Executor;
            
            let pool = SqlitePoolOptions::new()
                .connect(&format!("sqlite:{}", db_path_str))
                .await
                .unwrap();
            
            // Create table and insert test data
            pool.execute(
                "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT, age INTEGER)"
            ).await.unwrap();
            
            pool.execute(
                "INSERT INTO users (name, age) VALUES ('Alice', 30), ('Bob', 25), ('Charlie', 35)"
            ).await.unwrap();
        });
        
        // Test DatabaseAccess
        let access = DatabaseAccess::new(
            format!("sqlite://{}", db_path_str),
            DatabaseType::SQLite,
            String::new(),
            String::new(),
            "SELECT * FROM users ORDER BY id".to_string(),
        );
        
        let mut reader = access.get_reader().unwrap();
        let mut content = String::new();
        reader.read_to_string(&mut content).unwrap();
        
        // Verify CSV output
        assert!(content.contains("id,name,age"));
        assert!(content.contains("Alice"));
        assert!(content.contains("Bob"));
        assert!(content.contains("Charlie"));
        assert!(content.contains("30"));
        assert!(content.contains("25"));
        assert!(content.contains("35"));
    }
    
    #[test]
    #[ignore]
    fn test_sqlite_database_access_with_null_values() {
        use std::io::Read;
        
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("test_null.db");
        let db_path_str = db_path.to_str().unwrap();
        
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            use sqlx::sqlite::SqlitePoolOptions;
            use sqlx::Executor;
            
            let pool = SqlitePoolOptions::new()
                .connect(&format!("sqlite:{}", db_path_str))
                .await
                .unwrap();
            
            pool.execute(
                "CREATE TABLE products (id INTEGER PRIMARY KEY, name TEXT, price REAL, description TEXT)"
            ).await.unwrap();
            
            pool.execute(
                "INSERT INTO products (name, price, description) VALUES ('Product1', 19.99, 'Description1')"
            ).await.unwrap();
            
            pool.execute(
                "INSERT INTO products (name, price, description) VALUES ('Product2', NULL, NULL)"
            ).await.unwrap();
        });
        
        let access = DatabaseAccess::new(
            format!("sqlite://{}", db_path_str),
            DatabaseType::SQLite,
            String::new(),
            String::new(),
            "SELECT * FROM products ORDER BY id".to_string(),
        );
        
        let mut reader = access.get_reader().unwrap();
        let mut content = String::new();
        reader.read_to_string(&mut content).unwrap();
        
        // Verify CSV output handles NULL values
        assert!(content.contains("id,name,price,description"));
        assert!(content.contains("Product1"));
        assert!(content.contains("Product2"));
    }
    
    #[test]
    #[ignore]
    fn test_sqlite_database_access_empty_result() {
        use std::io::Read;
        
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("test_empty.db");
        let db_path_str = db_path.to_str().unwrap();
        
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            use sqlx::sqlite::SqlitePoolOptions;
            use sqlx::Executor;
            
            let pool = SqlitePoolOptions::new()
                .connect(&format!("sqlite:{}", db_path_str))
                .await
                .unwrap();
            
            pool.execute(
                "CREATE TABLE empty_table (id INTEGER PRIMARY KEY, value TEXT)"
            ).await.unwrap();
        });
        
        let access = DatabaseAccess::new(
            format!("sqlite://{}", db_path_str),
            DatabaseType::SQLite,
            String::new(),
            String::new(),
            "SELECT * FROM empty_table".to_string(),
        );
        
        let mut reader = access.get_reader().unwrap();
        let mut content = String::new();
        reader.read_to_string(&mut content).unwrap();
        
        // Should only contain headers, no data rows
        assert!(content.is_empty() || content.trim().is_empty());
    }
    
    #[test]
    #[ignore]
    fn test_sqlite_database_access_invalid_query() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("test_invalid.db");
        let db_path_str = db_path.to_str().unwrap();
        
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            use sqlx::sqlite::SqlitePoolOptions;
            
            let _pool = SqlitePoolOptions::new()
                .connect(&format!("sqlite:{}", db_path_str))
                .await
                .unwrap();
        });
        
        let access = DatabaseAccess::new(
            format!("sqlite://{}", db_path_str),
            DatabaseType::SQLite,
            String::new(),
            String::new(),
            "SELECT * FROM nonexistent_table".to_string(),
        );
        
        // Should return an error
        assert!(access.get_reader().is_err());
    }
    
    #[test]
    #[ignore]
    fn test_sqlite_database_access_connection_string_parsing() {
        let access = DatabaseAccess::new(
            "jdbc:sqlite:/path/to/database.db".to_string(),
            DatabaseType::SQLite,
            String::new(),
            String::new(),
            "SELECT 1".to_string(),
        );
        
        let conn_str = access.connection_string();
        assert!(conn_str.starts_with("sqlite:"));
        assert!(conn_str.contains("/path/to/database.db"));
    }
    
    #[test]
    fn test_mysql_connection_string_building() {
        let access = DatabaseAccess::new(
            "mysql://localhost/testdb".to_string(),
            DatabaseType::MySQL,
            "user".to_string(),
            "pass".to_string(),
            "SELECT 1".to_string(),
        );
        
        let conn_str = access.connection_string();
        assert!(conn_str.contains("mysql://"));
        assert!(conn_str.contains("user:pass"));
        assert!(conn_str.contains("localhost"));
        assert!(conn_str.contains("testdb"));
    }
    
    #[test]
    fn test_postgres_connection_string_building() {
        let access = DatabaseAccess::new(
            "postgresql://localhost/testdb".to_string(),
            DatabaseType::PostgreSQL,
            "user".to_string(),
            "pass".to_string(),
            "SELECT 1".to_string(),
        );
        
        let conn_str = access.connection_string();
        assert!(conn_str.contains("postgresql://"));
        assert!(conn_str.contains("user:pass"));
        assert!(conn_str.contains("localhost"));
        assert!(conn_str.contains("testdb"));
    }
    
    #[test]
    fn test_extract_host_and_database() {
        let access = DatabaseAccess::new(
            "mysql://myhost:3306/mydb".to_string(),
            DatabaseType::MySQL,
            "user".to_string(),
            "pass".to_string(),
            "SELECT 1".to_string(),
        );
        
        let (host, db) = access.extract_host_and_database();
        assert_eq!(host, "myhost:3306");
        assert_eq!(db, "mydb");
    }
    
    #[test]
    fn test_extract_host_and_database_jdbc_format() {
        let access = DatabaseAccess::new(
            "jdbc:postgresql://dbserver/production".to_string(),
            DatabaseType::PostgreSQL,
            "admin".to_string(),
            "secret".to_string(),
            "SELECT 1".to_string(),
        );
        
        let (host, db) = access.extract_host_and_database();
        assert_eq!(host, "dbserver");
        assert_eq!(db, "production");
    }
    
    #[test]
    fn test_mssql_not_supported() {
        let access = DatabaseAccess::new(
            "sqlserver://localhost/testdb".to_string(),
            DatabaseType::MSSQL,
            "user".to_string(),
            "pass".to_string(),
            "SELECT 1".to_string(),
        );
        
        // MSSQL should return an error as it's not supported by sqlx
        let result = access.get_reader();
        assert!(result.is_err());
        if let Err(RmlError::Database(msg)) = result {
            assert!(msg.contains("MSSQL is not yet supported"));
        }
    }
    
    // MySQL integration test (requires running MySQL instance)
    #[test]
    #[ignore]
    fn test_mysql_database_access() {
        use std::io::Read;
        
        // This test requires a MySQL instance running on localhost
        // with a test database and user credentials
        let access = DatabaseAccess::new(
            "mysql://localhost/test".to_string(),
            DatabaseType::MySQL,
            "test_user".to_string(),
            "test_password".to_string(),
            "SELECT 1 as num, 'test' as text".to_string(),
        );
        
        let mut reader = access.get_reader().unwrap();
        let mut content = String::new();
        reader.read_to_string(&mut content).unwrap();
        
        assert!(content.contains("num,text"));
        assert!(content.contains("1,test"));
    }
    
    // PostgreSQL integration test (requires running PostgreSQL instance)
    #[test]
    #[ignore]
    fn test_postgres_database_access() {
        use std::io::Read;
        
        // This test requires a PostgreSQL instance running on localhost
        // with a test database and user credentials
        let access = DatabaseAccess::new(
            "postgresql://localhost/test".to_string(),
            DatabaseType::PostgreSQL,
            "test_user".to_string(),
            "test_password".to_string(),
            "SELECT 1 as num, 'test' as text".to_string(),
        );
        
        let mut reader = access.get_reader().unwrap();
        let mut content = String::new();
        reader.read_to_string(&mut content).unwrap();
        
        assert!(content.contains("num,text"));
        assert!(content.contains("1,test"));
    }
}
