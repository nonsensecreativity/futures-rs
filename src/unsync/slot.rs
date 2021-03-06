//! An unbounded channel that only stores last value sent

use std::rc::{Rc, Weak};
use std::cell::RefCell;

use task::{self, Task};
use {Sink, Stream, AsyncSink, Async, Poll, StartSend};

/// Slot is very similar to unbounded channel but only stores last value sent
///
/// I.e. if you want to send some value between from producer to a consumer
/// and if consumer is slow it should skip old values, the slot is
/// a structure for the task.

/// The transmission end of a channel which is used to send values
///
/// If the receiver is not fast enough only the last value is preserved and
/// other ones are discarded.
#[derive(Debug)]
pub struct Sender<T> {
    inner: Weak<RefCell<Inner<T>>>,
}

/// The receiving end of a channel which preserves only the last value
#[derive(Debug)]
pub struct Receiver<T> {
    inner: Rc<RefCell<Inner<T>>>,
}

/// Error type for sending, used when the receiving end of a channel is
/// dropped
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct SendError<T>(T);

#[derive(Debug)]
struct Inner<T> {
    value: Option<T>,
    task: Option<Task>,
}

impl<T> Sender<T> {
    /// Sets the new new value of the stream and notifies the consumer if any.
    ///
    /// This function will store the `value` provided as the current value for
    /// this channel, replacing any previous value that may have been there. If
    /// the receiver may still be able to receive this message, then `Ok` is
    /// returned with the previous value that was in this channel.
    ///
    /// If `Ok(Some)` is returned then this value overwrote a previous value,
    /// and the value was never received by the receiver. If `Ok(None)` is
    /// returned, then no previous value was found and the `value` is queued up
    /// to be received by the receiver.
    ///
    /// # Errors
    ///
    /// This function will return an `Err` if the receiver has gone away and
    /// it's impossible to send this value to the receiver. The error returned
    /// retains ownership of the `value` provided and can be extracted, if
    /// necessary.
    pub fn swap(&self, value: T) -> Result<Option<T>, SendError<T>> {
        let result;
        // Do this step first so that the cell is dropped when
        // `unpark` is called
        let task = {
            if let Some(ref cell) = self.inner.upgrade() {
                let mut inner = cell.borrow_mut();
                result = inner.value.take();
                inner.value = Some(value);
                inner.task.take()
            } else {
                return Err(SendError(value));
            }
        };
        if let Some(task) = task {
            task.notify();
        }
        return Ok(result);
    }
}

impl<T> Sink for Sender<T> {
    type SinkItem = T;
    type SinkError = SendError<T>;
    fn start_send(&mut self, item: T) -> StartSend<T, SendError<T>> {
        self.swap(item)?;
        Ok(AsyncSink::Ready)
    }
    fn poll_complete(&mut self) -> Poll<(), Self::SinkError> {
        Ok(Async::Ready(()))
    }
    fn close(&mut self) -> Poll<(), Self::SinkError> {
        // Do this step first so that the cell is dropped *and*
        // weakref is dropped when `unpark` is called
        let task = self.inner.upgrade()
            .and_then(|inner| inner.borrow_mut().task.take());
        self.inner = Weak::new();
        // notify on any drop of a sender, so eventually receiver wakes up
        // when there are no senders and closes the stream
        if let Some(task) = task {
            task.notify();
        }
        Ok(Async::Ready(()))
    }
}

impl<T> Drop for Sender<T> {
    fn drop(&mut self) {
        self.close().ok();
    }
}

impl<T> Stream for Receiver<T> {
    type Item = T;
    type Error = ();  // actually void
    fn poll(&mut self) -> Poll<Option<Self::Item>, Self::Error> {
        let result = {
            let mut inner = self.inner.borrow_mut();
            if inner.value.is_none() {
                if Rc::weak_count(&self.inner) == 0 {
                    // no senders, terminate the stream
                    return Ok(Async::Ready(None));
                } else {
                    inner.task = Some(task::current());
                }
            }
            inner.value.take()
        };
        match result {
            Some(value) => Ok(Async::Ready(Some(value))),
            None => Ok(Async::NotReady),
        }
    }
}

/// Creates an in-memory Stream which only preserves last value
///
/// This method is somewhat similar to `channel(1)` but instead of preserving
/// first value sent (and erroring on sender side) it replaces value if
/// consumer is not fast enough and preserves last values sent on any
/// poll of a stream.
///
/// # Example
///
/// ```
/// use std::thread;
/// use futures::prelude::*;
/// use futures::stream::iter_ok;
/// use futures::unsync::slot;
///
/// let (tx, rx) = slot::channel::<i32>();
///
/// tx.send_all(iter_ok(vec![1, 2, 3])).wait();
///
/// let received = rx.collect().wait().unwrap();
/// assert_eq!(received, vec![3]);
/// ```
pub fn channel<T>() -> (Sender<T>, Receiver<T>) {
    let inner = Rc::new(RefCell::new(Inner {
        value: None,
        task: None,
    }));
    return (Sender { inner: Rc::downgrade(&inner) },
            Receiver { inner: inner });
}

impl<T> Clone for Sender<T> {
    fn clone(&self) -> Sender<T> {
        Sender { inner: self.inner.clone() }
    }
}
