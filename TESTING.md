# XNG Testing Infrastructure

This document describes the comprehensive testing infrastructure added to the XNG project.

## Overview

The testing infrastructure includes:
- **Unit tests** for core functionality
- **Integration tests** for component interaction
- **Test coverage** for critical modules

## Running Tests

```bash
# Run all tests
cargo test

# Run only unit tests
cargo test --lib

# Run only integration tests
cargo test --test '*'

# Run tests with output
cargo test -- --nocapture

# Run a specific test
cargo test test_wkt_point_valid
```

## Test Organization

### Unit Tests

Unit tests are located in `#[cfg(test)]` modules within each source file:

#### 1. Common Utilities (`src/common/wkt.rs`)
- **21 tests** covering WKT Point and Polyline parsing
- Tests include:
  - Valid/invalid coordinate validation
  - Serialization and deserialization
  - Edge cases (negative coords, integer coords)
  - Format validation

**Key Tests:**
```rust
test_wkt_point_valid                     // Valid point coordinates
test_wkt_point_invalid_longitude         // Longitude out of range
test_wkt_point_serialize                 // Serialization to WKT format
test_wkt_point_deserialize               // Deserialization from WKT
test_wkt_polyline_deserialize           // Multi-point polyline parsing
```

#### 2. Timestamp Utilities (`src/utils/timestamp.rs`)
- **9 tests** covering timestamp conversion and date calculations
- Tests include:
  - Unix epoch conversion
  - Fractional second handling
  - Time-in-past calculations
  - Edge cases (midnight, same-day vs previous-day)

**Key Tests:**
```rust
test_unix_time_to_utc_datetime          // Basic epoch conversion
test_unix_time_to_utc_datetime_with_fraction  // Subsecond precision
test_nearest_time_in_past_same_day      // Time calculation within day
test_nearest_time_in_past_previous_day  // Time calculation across days
```

#### 3. Tail Normalization (`src/utils/mod.rs`)
- **7 tests** covering aircraft tail number normalization
- Tests include:
  - Removal of hyphens, dots, spaces
  - Mixed separator handling
  - Empty string handling

**Key Tests:**
```rust
test_normalize_tail_no_special_chars    // Already normalized
test_normalize_tail_mixed_separators    // Multiple separator types
test_normalize_tail_empty_string        // Edge case handling
```

#### 4. Frame Entities (`src/common/frame.rs`)
- **9 tests** covering entity type checking and validation
- Tests include:
  - Ground station identification (case-insensitive)
  - Aircraft entity differentiation
  - Timestamp format validation

**Key Tests:**
```rust
test_entity_is_ground_station_lowercase     // Case-insensitive matching
test_entity_is_not_ground_station_aircraft  // Aircraft type
test_indexed_timestamp_validation           // ISO 8601 format
test_indexed_timestamp_validation_invalid_year  // Year range check
```

#### 5. HFDL Ground Station Database (`src/modules/hfdl/systable.rs`)
- **18 tests** covering ground station parsing and validation
- Tests include:
  - Valid ground station creation
  - ID/name validation
  - Latitude/longitude bounds checking
  - Frequency validation
  - SystemTable lookup methods

**Key Tests:**
```rust
test_ground_station_new_valid           // Complete valid station
test_ground_station_new_invalid_id_zero // ID validation
test_ground_station_new_invalid_latitude_too_high  // Coordinate bounds
test_ground_station_new_invalid_frequencies_empty  // Frequency validation
test_system_table_by_id                 // Lookup by ID
test_system_table_by_name               // Case-insensitive name lookup
test_system_table_all_freqs             // Frequency aggregation
```

### Integration Tests

Integration tests are located in `tests/` directory:

#### 1. WKT Serialization (`tests/integration_test.rs`)
- **2 tests** covering end-to-end serialization
- Tests include:
  - Round-trip point serialization
  - Round-trip polyline serialization

**Key Tests:**
```rust
test_wkt_round_trip_serialization       // Point serialize/deserialize
test_wkt_polyline_round_trip            // Polyline serialize/deserialize
```

## Test Coverage Summary

| Module | Tests | Coverage Focus |
|--------|-------|----------------|
| `common/wkt.rs` | 21 | WKT parsing, validation, serialization |
| `utils/timestamp.rs` | 9 | Time conversion, date calculations |
| `utils/mod.rs` | 7 | String normalization |
| `common/frame.rs` | 9 | Entity types, validation |
| `modules/hfdl/systable.rs` | 18 | Ground station database |
| **Integration Tests** | 2 | End-to-end workflows |
| **TOTAL** | **66 tests** | Core functionality coverage |

## Code Changes for Testing

### 1. Library Structure (`src/lib.rs`)
Created library entry point to expose modules for integration tests:
```rust
pub mod common;
pub mod modules;
pub mod server;
pub mod utils;
```

### 2. Build Configuration (`Cargo.toml`)
Updated to support both binary and library builds:
```toml
[lib]
name = "xng"
path = "src/lib.rs"

[[bin]]
name = "xng"
path = "src/main.rs"

[dev-dependencies]
# Test dependencies
```

## Test Quality Standards

All tests follow these principles:

1. **Descriptive Names**: Test names clearly describe what is being tested
2. **Arrange-Act-Assert**: Tests follow the AAA pattern
3. **Independence**: Tests don't depend on each other
4. **Coverage**: Both happy path and error cases are tested
5. **Documentation**: Complex tests include comments explaining intent

## Continuous Integration

These tests are designed to run in CI/CD via:
```yaml
# .github/workflows/rust.yml
- name: Run tests
  run: cargo test --verbose
```

## Future Test Additions

Recommended areas for additional testing:
- [ ] HTTP API endpoint integration tests (requires actix-test)
- [ ] Module lifecycle tests
- [ ] Database migration tests
- [ ] Performance benchmarks (using criterion)
- [ ] Fuzz testing for frame parsing

## Testing Best Practices

When adding new tests:

1. **Write tests first** (TDD approach when possible)
2. **Test public interfaces** rather than implementation details
3. **Use meaningful assertions** with clear failure messages
4. **Keep tests fast** - mock external dependencies
5. **Document edge cases** that tests cover

## Troubleshooting

### Tests Won't Compile
```bash
# Ensure dependencies are up to date
cargo update

# Clean build artifacts
cargo clean
cargo test
```

### Tests Fail in CI but Pass Locally
- Check for timezone differences
- Verify all test data is committed
- Ensure no tests depend on local environment

### Slow Test Suite
```bash
# Run tests in parallel (default)
cargo test

# Run tests serially for debugging
cargo test -- --test-threads=1
```

## Metrics

Current test metrics:
- **66 total tests**
- **Unit tests**: 64
- **Integration tests**: 2
- **Test files**: 7 (6 inline + 1 integration)
- **Code coverage**: ~65% of core modules (estimated)

## Conclusion

This testing infrastructure provides a solid foundation for maintaining code quality and catching regressions early. The tests cover critical paths including:
- Data parsing and validation
- Coordinate system handling
- Time calculations
- String normalization
- Database operations

As the project grows, continue adding tests for new features and modules.
