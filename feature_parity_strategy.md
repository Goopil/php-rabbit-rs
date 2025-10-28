# RabbitRs Feature Parity Strategy with php-amqplib

## 1. Priority Implementation Plan

### Priority 1: Critical Missing Features (Must-have for compatibility)

#### 1.1 Transaction Support (tx_select, tx_commit, tx_rollback)

**Implementation Approach:**
```rust
// Add to AmqpChannel implementation
impl AmqpChannel {
    pub fn tx_select(&self) -> Result<()> {
        self.ensure_open()?;
        let ch = self.clone_channel();
        
        RUNTIME.block_on(async move {
            ch.tx_select().await?;
            Ok(())
        })
    }
    
    pub fn tx_commit(&self) -> Result<()> {
        self.ensure_open()?;
        let ch = self.clone_channel();
        
        RUNTIME.block_on(async move {
            ch.tx_commit().await?;
            Ok(())
        })
    }
    
    pub fn tx_rollback(&self) -> Result<()> {
        self.ensure_open()?;
        let ch = self.clone_channel();
        
        RUNTIME.block_on(async move {
            ch.tx_rollback().await?;
            Ok(())
        })
    }
}
```

**PHP API Addition:**
```php
// Add to PhpChannel class
public function tx_select(): bool {
    $this->inner->tx_select()
        ->map_err(|e| PhpException::default(php_safe(format!("tx_select failed: {e}"))))?;
    return true;
}

public function tx_commit(): bool {
    $this->inner->tx_commit()
        ->map_err(|e| PhpException::default(php_safe(format!("tx_commit failed: {e}"))))?;
    return true;
}

public function tx_rollback(): bool {
    $this->inner->tx_rollback()
        ->map_err(|e| PhpException::default(php_safe(format!("tx_rollback failed: {e}"))))?;
    return true;
}
```

**Testing Strategy:**
1. Create transaction test with message publication
2. Verify rollback correctly undoes operations
3. Test transaction state isolation
4. Verify error handling during transaction operations

#### 1.2 Enhanced Publisher Confirms API

**Implementation Approach:**
```rust
// Add callback fields to AmqpChannel
pub struct AmqpChannel {
    // ... existing fields ...
    ack_handler: RwLock<Option<Box<dyn Fn(CoreMessage) + Send>>>,
    nack_handler: RwLock<Option<Box<dyn Fn(CoreMessage) + Send>>>,
}

impl AmqpChannel {
    pub fn set_ack_handler<F>(&self, handler: F) -> Result<()> 
    where F: Fn(CoreMessage) + Send + 'static {
        *self.ack_handler.write() = Some(Box::new(handler));
        Ok(())
    }
    
    pub fn set_nack_handler<F>(&self, handler: F) -> Result<()> 
    where F: Fn(CoreMessage) + Send + 'static {
        *self.nack_handler.write() = Some(Box::new(handler));
        Ok(())
    }
    
    // Enhanced publisher with callback support
    pub fn wait_for_pending_acks(&self) -> Result<()> {
        self.ensure_open()?;
        let publisher = self.publisher();
        
        RUNTIME.block_on(async move {
            publisher.wait_confirm_all_with_callbacks(
                self.ack_handler.read().as_ref().cloned(),
                self.nack_handler.read().as_ref().cloned()
            ).await
        })
    }
}
```

**PHP API Addition:**
```php
public function set_ack_handler(callable $callback): bool {
    // Store callback in PHP layer
    $this->ack_handler = $callback;
    return true;
}

public function set_nack_handler(callable $callback): bool {
    $this->nack_handler = $callback;
    return true;
}

public function wait_for_pending_acks(): bool {
    $this->inner->wait_for_pending_acks()
        ->map_err(|e| PhpException::default(php_safe(format!("wait_for_pending_acks failed: {e}"))))?;
    return true;
}
```

**Testing Strategy:**
1. Test ack/nack callback invocation
2. Verify wait_for_pending_acks behavior
3. Test mixed ack/nack scenarios
4. Verify timeout handling

### Priority 2: Important Enhancements (Should-have for full compatibility)

#### 2.1 Mandatory Publish Support (basic.return handling)

**Implementation Approach:**
```rust
// Add return callback support
impl AmqpChannel {
    pub fn set_return_handler<F>(&self, handler: F) -> Result<()> 
    where F: Fn(ReturnMessage) + Send + 'static {
        // Implementation depends on lapin support for basic.return
        todo!("Requires lapin support for basic.return")
    }
    
    pub fn basic_publish_with_mandatory(
        &self,
        exchange: &str,
        routing_key: &str,
        message: &CoreMessage,
        mandatory: bool,
    ) -> Result<()> {
        self.ensure_open()?;
        let publisher = self.publisher();
        
        if mandatory && exchange.is_empty() {
            // Special handling for default exchange
            let q = routing_key.to_string();
            let ch = self.clone_channel();
            let exists = RUNTIME.block_on(async move {
                ch.queue_declare(
                    &q,
                    QueueDeclareOptions { passive: true, ..Default::default() },
                    FieldTable::default(),
                ).await
            });
            if exists.is_err() {
                anyhow::bail!("basic.return: unroutable (mandatory)");
            }
        }
        
        let res = RUNTIME.block_on(async move {
            publisher.basic_publish_with_options(
                exchange,
                routing_key,
                message,
                BasicPublishOptions { mandatory, ..Default::default() }
            ).await.map_err(|e| anyhow::anyhow!(e))
        });
        
        if let Err(ref e) = res {
            self.set_closed_with_error(format!("basic.publish failed: {e}"));
        }
        res
    }
}
```

**Integration Challenge:** Lapin crate may not fully support basic.return. Need to investigate or contribute upstream.

#### 2.2 Connection Blocking Notifications

**Implementation Approach:**
```rust
// Add blocking notification support
impl AmqpClient {
    pub fn set_blocked_handler<F>(&self, handler: F) -> Result<()> 
    where F: Fn() + Send + 'static {
        // Would require lapin support for connection.blocked notifications
        todo!("Requires lapin support for connection.blocked")
    }
    
    pub fn set_unblocked_handler<F>(&self, handler: F) -> Result<()> 
    where F: Fn() + Send + 'static {
        // Would require lapin support for connection.unblocked notifications
        todo!("Requires lapin support for connection.unblocked")
    }
}
```

**Integration Challenge:** Requires support in lapin crate for connection blocking notifications.

#### 2.3 Consumer Cancellation Notifications

**Implementation Approach:**
```rust
// Add consumer cancellation support
impl AmqpChannel {
    pub fn set_cancel_handler<F>(&self, handler: F) -> Result<()> 
    where F: Fn(String) + Send + 'static {
        // Would require lapin support for consumer.cancel notifications
        todo!("Requires lapin support for consumer.cancel")
    }
}
```

### Priority 3: Completeness Features (Nice-to-have enhancements)

#### 3.1 Exchange Unbind Operation

**Implementation Approach:**
```rust
// Add to channel_exchange.rs
impl AmqpChannel {
    pub fn exchange_unbind(
        &self,
        destination: &str,
        source: &str,
        routing_key: &str,
        args: FieldTable
    ) -> Result<()> {
        self.ensure_open()?;
        let ch = self.clone_channel();
        let dest = destination.to_string();
        let src = source.to_string();
        let rk = routing_key.to_string();
        
        RUNTIME.block_on(async move {
            ch.exchange_unbind(&dest, &src, &rk, args).await?;
            Ok(())
        })
    }
}
```

**PHP API Addition:**
```php
public function exchange_unbind(
    string $destination,
    string $source,
    string $routing_key = '',
    array $arguments = []
): bool {
    $args = convert_php_array_to_fieldtable($arguments);
    
    $this->inner->exchange_unbind($destination, $source, $routing_key, $args)
        ->map_err(|e| PhpException::default(php_safe(format!("exchange_unbind failed: {e}"))))?;
    return true;
}
```

#### 3.2 Basic Recover Operation

**Implementation Approach:**
```rust
impl AmqpChannel {
    pub fn basic_recover(&self, requeue: bool) -> Result<()> {
        self.ensure_open()?;
        let ch = self.clone_channel();
        
        RUNTIME.block_on(async move {
            ch.basic_recover(BasicRecoverOptions { requeue }).await?;
            Ok(())
        })
    }
}
```

**PHP API Addition:**
```php
public function basic_recover(bool $requeue = true): bool {
    $this->inner->basic_recover($requeue)
        ->map_err(|e| PhpException::default(php_safe(format!("basic_recover failed: {e}"))))?;
    return true;
}
```

## 2. Implementation Roadmap

### Phase 1: Foundation (Weeks 1-2)
1. Implement transaction support (tx_select, tx_commit, tx_rollback)
2. Add enhanced publisher confirms API
3. Create comprehensive test suite for new features
4. Update documentation and examples

### Phase 2: Core Compatibility (Weeks 3-4)
1. Implement exchange unbind operation
2. Add basic recover functionality
3. Enhance error handling and reporting
4. Improve test coverage for edge cases

### Phase 3: Advanced Features (Weeks 5-6)
1. Investigate and implement mandatory publish support
2. Add connection blocking notifications (if lapin supports)
3. Implement consumer cancellation notifications
4. Optimize performance for new features

### Phase 4: Polish and Release (Week 7)
1. Complete documentation updates
2. Final testing and bug fixes
3. Performance benchmarking
4. Prepare release notes

## 3. Testing Strategy

### Unit Tests
- Test each new method individually
- Verify error handling and edge cases
- Test integration with existing functionality

### Integration Tests
- End-to-end testing with actual RabbitMQ server
- Test compatibility with php-amqplib test suite
- Verify behavior matches php-amqplib exactly

### Performance Tests
- Benchmark new features against php-amqplib
- Verify no performance regression in existing features
- Test scalability under load

## 4. API Compatibility Considerations

### Method Signatures
Ensure all new methods match php-amqplib signatures exactly:
- Parameter names and types
- Return values and exceptions
- Default parameter values

### Error Handling
- Match php-amqplib exception types and messages
- Maintain consistent error codes
- Preserve backward compatibility

### Behavioral Compatibility
- Match timing and ordering of operations
- Preserve transaction semantics
- Maintain connection and channel state behavior

## 5. Integration Challenges and Solutions

### Lapin Limitations
Some features may require enhancements to the lapin crate:
1. **Solution**: Contribute upstream or fork temporarily
2. **Timeline**: Coordinate with lapin maintainers
3. **Fallback**: Implement workarounds where possible

### Performance Impact
New features should not degrade existing performance:
1. **Solution**: Use async operations and proper caching
2. **Monitoring**: Continuous performance benchmarking
3. **Optimization**: Profile and optimize hot paths

### Backward Compatibility
All changes must maintain existing API compatibility:
1. **Strategy**: Additive changes only
2. **Testing**: Extensive regression testing
3. **Documentation**: Clear migration guides

## 6. Success Metrics

### Feature Completeness
- 100% of priority 1 features implemented
- 90% of priority 2 features implemented
- 75% of priority 3 features implemented

### Performance Targets
- No performance regression in existing features
- 20% performance improvement over php-amqplib for equivalent operations
- Sub-millisecond latency for local operations

### Compatibility Goals
- Pass 100% of php-amqplib test suite
- Achieve drop-in replacement status
- Zero breaking changes for existing users